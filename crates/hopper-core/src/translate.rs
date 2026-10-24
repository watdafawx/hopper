//! Offline translation of mod changelogs and descriptions into English.
//!
//! Text never leaves the PC: each language has an OPUS-MT model
//! (Helsinki-NLP, CC-BY 4.0) that is downloaded once from Hugging Face,
//! when the user asks for it, and run here with candle.
//!
//! Hugging Face only has these models as old-style PyTorch checkpoints,
//! which candle can't read, so the download is converted once into an f16
//! safetensors file (half the size) and the checkpoint deleted.
//!
//! Translation works on markdown: only the runs of text between markdown
//! syntax are translated, so headings, lists, bold, links and code stay as
//! they were. Translated sentences are cached on disk per language.

use anyhow::{Context as _, Result, bail};
use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::marian;
use std::collections::HashMap;
use std::io::{BufReader, Read as _};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// A language Hopper can translate into English.
#[derive(Debug, PartialEq, Eq)]
pub struct Lang {
    /// ISO 639-1 code, also the settings key.
    pub code: &'static str,
    /// English name, for the UI.
    pub name: &'static str,
    /// The Hugging Face revision the files are fetched at, so they can't
    /// change under us.
    revision: &'static str,
    /// Bytes downloaded (the checkpoint plus the small files), for the UI.
    pub download: u64,
    whatlang: whatlang::Lang,
}

const fn lang(code: &'static str, name: &'static str, revision: &'static str, mb: u64, whatlang: whatlang::Lang) -> Lang {
    Lang { code, name, revision, download: mb << 20, whatlang }
}

/// Every language with a model, the common ones in mod text first.
pub static LANGS: &[Lang] = &[
    lang("zh", "Chinese", "cf109095479db38d6df799875e34039d4938aaa6", 300, whatlang::Lang::Cmn),
    lang("ru", "Russian", "fbd6dc73284f95536648512cc21d57f19191961a", 296, whatlang::Lang::Rus),
    lang("ja", "Japanese", "0770961a39ba6bd66305b149c3f4110bcafca2e6", 292, whatlang::Lang::Jpn),
    lang("ko", "Korean", "e42d1f41b66194e6d10512f8a27bebc1f4f5097e", 300, whatlang::Lang::Kor),
    lang("uk", "Ukrainian", "1b9a2a0d1aa2a329d8476d2be5c7b0b27d5397b0", 296, whatlang::Lang::Ukr),
    lang("de", "German", "1a922f3b32a8e809e17a47d4b32142d8105924e5", 287, whatlang::Lang::Deu),
    lang("fr", "French", "c4aed37b318c763fd177aa449b44e3b783cc6c02", 290, whatlang::Lang::Fra),
    lang("es", "Spanish", "c96e2c5399ebfae4fc43d9669556b9afa74bb69d", 300, whatlang::Lang::Spa),
    lang("it", "Italian", "42556a0848fc726f4d27399f20b19ff6f01afe11", 290, whatlang::Lang::Ita),
    lang("pl", "Polish", "7f2bb874fdfb6139f9842b91a9b75c4a6c93401c", 290, whatlang::Lang::Pol),
    lang("tr", "Turkish", "19c65427cc2af5f191337d4899e0348c4af25902", 290, whatlang::Lang::Tur),
    lang("vi", "Vietnamese", "c8d2853e77f5fae31124d993e0b35176b1c8914e", 290, whatlang::Lang::Vie),
];

pub fn by_code(code: &str) -> Option<&'static Lang> {
    LANGS.iter().find(|l| l.code == code)
}

// ---- detection --------------------------------------------------------------------

fn is_han(c: char) -> bool {
    matches!(c, '\u{4E00}'..='\u{9FFF}' | '\u{3400}'..='\u{4DBF}' | '\u{F900}'..='\u{FAFF}')
}
fn is_kana(c: char) -> bool {
    matches!(c, '\u{3040}'..='\u{30FF}' | '\u{31F0}'..='\u{31FF}')
}
fn is_hangul(c: char) -> bool {
    matches!(c, '\u{AC00}'..='\u{D7AF}' | '\u{1100}'..='\u{11FF}' | '\u{3130}'..='\u{318F}')
}
fn is_cyrillic(c: char) -> bool {
    matches!(c, '\u{0400}'..='\u{04FF}')
}

/// Whether `text` holds something to translate for `lang`: its own script
/// for the CJK and Cyrillic ones (so the English half of a bilingual
/// changelog stays as it is), any letter for the Latin-script ones.
fn wants(lang: &Lang, text: &str) -> bool {
    match lang.code {
        "zh" => text.chars().any(is_han),
        "ja" => text.chars().any(|c| is_kana(c) || is_han(c)),
        "ko" => text.chars().any(is_hangul),
        "ru" | "uk" => text.chars().any(is_cyrillic),
        _ => text.chars().any(char::is_alphabetic),
    }
}

/// The language of `md` when it isn't English and Hopper has a model for
/// it. A bilingual text counts as its non-English part.
pub fn detect(md: &str) -> Option<&'static Lang> {
    let text: String = pieces(md)
        .into_iter()
        .map(|r| &md[r])
        .collect::<Vec<_>>()
        .join("\n");
    let count = |f: fn(char) -> bool| text.chars().filter(|&c| f(c)).count();
    let (kana, han, hangul, cyr) = (count(is_kana), count(is_han), count(is_hangul), count(is_cyrillic));
    // A few characters (a name, an emoji-like symbol) don't make a language.
    if kana >= 5 {
        return by_code("ja");
    }
    if hangul >= 5 {
        return by_code("ko");
    }
    if han >= 8 {
        return by_code("zh");
    }
    if cyr >= 20 {
        let cyr_text: String = text.chars().filter(|&c| is_cyrillic(c) || c.is_whitespace()).collect();
        let uk = whatlang::detect(&cyr_text).is_some_and(|i| i.lang() == whatlang::Lang::Ukr);
        return by_code(if uk { "uk" } else { "ru" });
    }
    let info = whatlang::detect(&text)?;
    if info.lang() == whatlang::Lang::Eng || !info.is_reliable() || text.chars().filter(|c| c.is_alphabetic()).count() < 40 {
        return None;
    }
    LANGS.iter().find(|l| l.whatlang == info.lang() && !matches!(l.code, "zh" | "ja" | "ko" | "ru" | "uk"))
}

// ---- markdown pieces --------------------------------------------------------------

/// Byte ranges of the plain text in `md`: everything but markdown syntax,
/// code, URLs and HTML tags, trimmed.
fn pieces(md: &str) -> Vec<std::ops::Range<usize>> {
    let mut out = Vec::new();
    let mut fenced = false;
    let mut line_start = 0;
    for line in md.split_inclusive('\n') {
        let base = line_start;
        line_start += line.len();
        let t = line.trim_start();
        if t.starts_with("```") || t.starts_with("~~~") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        let body = line.trim_end_matches(['\n', '\r']);
        let start = base + prefix_len(body);
        scan_inline(md, start, base + body.len(), &mut out);
    }
    out
}

/// Length of a line's block markers: indentation, `#`s, `>`, list bullets
/// and numbers.
fn prefix_len(line: &str) -> usize {
    let b = line.as_bytes();
    let mut i = 0;
    loop {
        let before = i;
        while i < b.len() && b[i] == b' ' {
            i += 1;
        }
        let mut j = i;
        while j < b.len() && b[j] == b'#' {
            j += 1;
        }
        if j > i && j - i <= 6 && b.get(j) == Some(&b' ') {
            i = j + 1;
        } else if matches!(b.get(i), Some(b'>')) {
            i += 1;
        } else if matches!(b.get(i), Some(b'-' | b'*' | b'+')) && b.get(i + 1) == Some(&b' ') {
            i += 2;
        } else {
            let mut k = i;
            while k < b.len() && b[k].is_ascii_digit() {
                k += 1;
            }
            if k > i && matches!(b.get(k), Some(b'.' | b')')) && b.get(k + 1) == Some(&b' ') {
                i = k + 2;
            }
        }
        if i == before {
            return i;
        }
    }
}

fn scan_inline(md: &str, from: usize, to: usize, out: &mut Vec<std::ops::Range<usize>>) {
    let s = &md[from..to];
    let b = s.as_bytes();
    let mut run = 0; // start of the current text run (offset in s)
    let mut i = 0;
    let push = |out: &mut Vec<std::ops::Range<usize>>, a: usize, z: usize| {
        let piece = &s[a..z];
        let lead = piece.len() - piece.trim_start().len();
        let trail = piece.len() - piece.trim_end().len();
        if lead + trail < piece.len() {
            out.push(from + a + lead..from + z - trail);
        }
    };
    while i < b.len() {
        let c = b[i];
        match c {
            b'`' => {
                push(out, run, i);
                // Skip the code span to its closing backticks.
                let ticks = b[i..].iter().take_while(|&&x| x == b'`').count();
                let fence = &s[i..i + ticks];
                i += ticks;
                i = s[i..].find(fence).map_or(b.len(), |p| i + p + ticks);
                run = i;
            }
            b'\\' => {
                push(out, run, i);
                i = (i + 2).min(b.len());
                while i < b.len() && !s.is_char_boundary(i) {
                    i += 1;
                }
                run = i;
            }
            b']' if b.get(i + 1) == Some(&b'(') => {
                push(out, run, i);
                // The link's URL, to its closing parenthesis.
                let mut depth = 0;
                let mut j = i + 1;
                while j < b.len() {
                    match b[j] {
                        b'(' => depth += 1,
                        b')' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    j += 1;
                }
                i = (j + 1).min(b.len());
                run = i;
            }
            b'<' => {
                push(out, run, i);
                i = s[i..].find('>').map_or(b.len(), |p| i + p + 1);
                run = i;
            }
            b'*' | b'_' | b'[' | b']' | b'|' | b'!' | b'~' => {
                // `_` and `!` only count as syntax next to other syntax or
                // at a word edge, not inside snake_case or "Hi!".
                let syntax = match c {
                    b'_' => {
                        let prev = i.checked_sub(1).map(|p| b[p]);
                        let next = b.get(i + 1).copied();
                        !(prev.is_some_and(|p| p.is_ascii_alphanumeric()) && next.is_some_and(|n| n.is_ascii_alphanumeric()))
                    }
                    b'!' => b.get(i + 1) == Some(&b'['),
                    _ => true,
                };
                if syntax {
                    push(out, run, i);
                    run = i + 1;
                }
                i += 1;
            }
            b'h' if s[i..].starts_with("http://") || s[i..].starts_with("https://") => {
                push(out, run, i);
                i += s[i..].find(char::is_whitespace).unwrap_or(b.len() - i);
                run = i;
            }
            _ => i += 1,
        }
    }
    push(out, run, b.len());
}

/// `text` cut into sentences, each with its closing punctuation.
fn sentences(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let end = i + c.len_utf8();
        let cut = match c {
            '。' | '！' | '？' | '；' | '!' | '?' => true,
            '.' => chars.peek().is_none_or(|(_, n)| n.is_whitespace()),
            _ => false,
        };
        // Very long runs without punctuation get cut at commas.
        let long = end - start > 600 && matches!(c, '，' | ',' | '、');
        if cut || long {
            let s = text[start..end].trim();
            if !s.is_empty() {
                out.push(s);
            }
            start = end;
        }
    }
    let s = text[start..].trim();
    if !s.is_empty() {
        out.push(s);
    }
    out
}

// ---- files ------------------------------------------------------------------------

fn root() -> Option<PathBuf> {
    crate::paths::dirs().map(|d| d.data_dir().join("translate"))
}

fn model_dir(lang: &Lang) -> Option<PathBuf> {
    root().map(|r| r.join(format!("opus-mt-{}-en", lang.code)))
}

const FILES: [&str; 3] = ["config.json", "source.spm", "vocab.json"];
const WEIGHTS: &str = "model-f16.safetensors";

/// Whether `lang`'s model is downloaded and ready.
pub fn installed(lang: &Lang) -> bool {
    model_dir(lang).is_some_and(|d| FILES.iter().chain([&WEIGHTS]).all(|f| d.join(f).is_file()))
}

/// The downloaded languages with the disk space each takes.
pub fn installed_list() -> Vec<(&'static Lang, u64)> {
    LANGS
        .iter()
        .filter(|l| installed(l))
        .map(|l| (l, model_dir(l).map(|d| crate::paths::dir_size(&d)).unwrap_or(0)))
        .collect()
}

/// Deletes `lang`'s model and its cached translations.
pub fn remove(lang: &Lang) -> Result<()> {
    unload(lang);
    if let Some(d) = model_dir(lang)
        && d.exists()
    {
        std::fs::remove_dir_all(&d).with_context(|| format!("remove {}", d.display()))?;
    }
    if let Some(r) = root() {
        let _ = std::fs::remove_file(r.join(format!("cache-{}.json", lang.code)));
    }
    if let Some(m) = CACHES.lock().unwrap().as_mut() {
        m.remove(lang.code);
    }
    Ok(())
}

/// Downloads and converts `lang`'s model. `progress` gets (bytes, total).
/// Stops with "Cancelled" when `job` is cancelled; nothing half-made stays.
pub fn download(lang: &Lang, job: Option<&crate::jobs::Job>, mut progress: impl FnMut(u64, u64)) -> Result<()> {
    if installed(lang) {
        return Ok(());
    }
    let dir = model_dir(lang).context("no data folder")?;
    let part = dir.with_extension("part");
    let _ = std::fs::remove_dir_all(&part);
    std::fs::create_dir_all(&part)?;
    let r = (|| {
        let url = |f: &str| format!("https://huggingface.co/Helsinki-NLP/opus-mt-{}-en/resolve/{}/{f}", lang.code, lang.revision);
        let total = lang.download;
        let mut done = 0;
        for f in FILES {
            crate::cf::download_checked(&url(f), &part.join(f), &Default::default(), job, |n| progress(done + n, total))?;
            done += std::fs::metadata(part.join(f)).map(|m| m.len()).unwrap_or(0);
        }
        let bin = part.join("pytorch_model.bin");
        crate::cf::download_checked(&url("pytorch_model.bin"), &bin, &Default::default(), job, |n| progress(done + n, total))?;
        progress(total, total);
        convert(&bin, &part.join(WEIGHTS))?;
        std::fs::remove_file(&bin)?;
        // Prove it loads before calling it installed.
        Engine::load(lang, &part)?;
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::rename(&part, &dir)?;
        anyhow::Ok(())
    })();
    if r.is_err() {
        let _ = std::fs::remove_dir_all(&part);
    }
    r
}

/// Reads an old-style (pre zip) PyTorch checkpoint and saves its tensors
/// as f16 safetensors.
fn convert(bin: &Path, out: &Path) -> Result<()> {
    use candle_core::pickle::{Object, Stack};
    let mut r = BufReader::new(std::fs::File::open(bin)?);
    let next = |r: &mut BufReader<std::fs::File>| -> Result<Object> {
        let mut st = Stack::empty();
        st.read_loop(r)?;
        Ok(st.finalize()?)
    };
    // Magic number, format version, system info, then the state dict.
    next(&mut r)?;
    next(&mut r)?;
    next(&mut r)?;
    let Object::Dict(entries) = next(&mut r)? else {
        bail!("checkpoint holds no state dict");
    };
    let infos: Vec<_> = entries
        .into_iter()
        .filter_map(|(k, v)| v.into_tensor_info(k, Path::new("s")).ok().flatten())
        .collect();
    // Then the storage keys, and each storage as an element count + data.
    let keys: Vec<String> = match next(&mut r)? {
        Object::List(l) | Object::Tuple(l) => l.into_iter().filter_map(|o| o.unicode().ok()).collect(),
        _ => bail!("checkpoint has no storage list"),
    };
    let mut storages: HashMap<String, Vec<u8>> = HashMap::new();
    for key in keys {
        let path = format!("s/{key}");
        let elem = infos.iter().find(|i| i.path == path).map_or(4, |i| i.dtype.size_in_bytes());
        let mut n = [0u8; 8];
        r.read_exact(&mut n)?;
        let mut data = vec![0u8; u64::from_le_bytes(n) as usize * elem];
        r.read_exact(&mut data)?;
        storages.insert(path, data);
    }
    let mut tensors = HashMap::new();
    for info in infos {
        // The embeddings are one tied matrix that candle reads once, as
        // `model.shared`; positions are computed. Skipping the copies
        // halves the file.
        if info.name.ends_with("embed_tokens.weight") || info.name.ends_with("embed_positions.weight") || info.name == "lm_head.weight" {
            continue;
        }
        let data = storages.get(&info.path).with_context(|| format!("missing storage for {}", info.name))?;
        let l = &info.layout;
        if !l.is_contiguous() {
            bail!("{} isn't contiguous", info.name);
        }
        let start = l.start_offset();
        let len = l.shape().elem_count() * info.dtype.size_in_bytes();
        let bytes = data.get(start..start + len).with_context(|| format!("{} is cut short", info.name))?;
        let t = Tensor::from_raw_buffer(bytes, info.dtype, l.dims(), &Device::Cpu)?;
        let t = if info.dtype.is_float() { t.to_dtype(DType::F16)? } else { t };
        tensors.insert(info.name, t);
    }
    candle_core::safetensors::save(&tensors, out)?;
    Ok(())
}

// ---- tokenizer --------------------------------------------------------------------

/// A minimal protobuf reader for SentencePiece's model file.
struct Proto<'a>(&'a [u8]);

impl<'a> Proto<'a> {
    fn varint(&mut self) -> Result<u64> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let (&b, rest) = self.0.split_first().context("truncated varint")?;
            self.0 = rest;
            v |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return Ok(v);
            }
        }
        bail!("bad varint")
    }

    /// The next field: number, and its bytes (`LEN`) or value.
    fn field(&mut self) -> Result<Option<(u64, Field<'a>)>> {
        if self.0.is_empty() {
            return Ok(None);
        }
        let key = self.varint()?;
        let f = match key & 7 {
            0 => Field::Int(self.varint()?),
            1 => Field::Fixed(self.take(8)?),
            2 => {
                let n = self.varint()? as usize;
                Field::Bytes(self.take(n)?)
            }
            5 => Field::Fixed(self.take(4)?),
            w => bail!("wire type {w}"),
        };
        Ok(Some((key >> 3, f)))
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if n > self.0.len() {
            bail!("truncated field");
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Ok(a)
    }
}

enum Field<'a> {
    Int(u64),
    Fixed(&'a [u8]),
    Bytes(&'a [u8]),
}

/// The source-side SentencePiece model as a `tokenizers` Unigram, the way
/// Hugging Face converts them.
fn tokenizer(spm: &[u8]) -> Result<tokenizers::Tokenizer> {
    let mut vocab = Vec::new();
    let mut unk = None;
    let mut charsmap: &[u8] = &[];
    let mut p = Proto(spm);
    while let Some((n, f)) = p.field()? {
        match (n, f) {
            (1, Field::Bytes(b)) => {
                let (mut piece, mut score, mut kind) = (String::new(), 0f64, 1);
                let mut q = Proto(b);
                while let Some((n, f)) = q.field()? {
                    match (n, f) {
                        (1, Field::Bytes(s)) => piece = String::from_utf8_lossy(s).into_owned(),
                        (2, Field::Fixed(x)) if x.len() == 4 => score = f32::from_le_bytes(x.try_into()?) as f64,
                        (3, Field::Int(k)) => kind = k,
                        _ => {}
                    }
                }
                if kind == 2 {
                    unk = Some(vocab.len());
                }
                vocab.push((piece, score));
            }
            (3, Field::Bytes(b)) => {
                let mut q = Proto(b);
                while let Some((n, f)) = q.field()? {
                    if let (2, Field::Bytes(m)) = (n, f) {
                        charsmap = m;
                    }
                }
            }
            _ => {}
        }
    }
    let model = tokenizers::models::unigram::Unigram::from(vocab, unk.or(Some(0)), false).map_err(anyhow::Error::msg)?;
    let mut tok = tokenizers::Tokenizer::new(model);
    if !charsmap.is_empty() {
        let norm = tokenizers::normalizers::precompiled::Precompiled::from(charsmap).map_err(anyhow::Error::msg)?;
        tok.with_normalizer(Some(norm));
    }
    tok.with_pre_tokenizer(Some(tokenizers::pre_tokenizers::metaspace::Metaspace::new(
        '▁',
        tokenizers::pre_tokenizers::metaspace::PrependScheme::Always,
        true,
    )));
    Ok(tok)
}

// ---- model ------------------------------------------------------------------------

struct Engine {
    code: &'static str,
    model: marian::MTModel,
    cfg: marian::Config,
    tok: tokenizers::Tokenizer,
    vocab: HashMap<String, u32>,
    /// id -> piece.
    pieces: Vec<String>,
    unk: u32,
}

impl Engine {
    fn load(lang: &Lang, dir: &Path) -> Result<Engine> {
        let cfg: marian::Config = serde_json::from_slice(&std::fs::read(dir.join("config.json"))?)?;
        let tok = tokenizer(&std::fs::read(dir.join("source.spm"))?)?;
        let vocab: HashMap<String, u32> = serde_json::from_slice(&std::fs::read(dir.join("vocab.json"))?)?;
        let mut pieces = vec![String::new(); vocab.values().max().map_or(0, |&m| m as usize + 1)];
        for (p, &id) in &vocab {
            pieces[id as usize] = p.clone();
        }
        let unk = vocab.get("<unk>").copied().unwrap_or(1);
        // SAFETY: the file is ours and isn't changed while mapped.
        let vb = unsafe { VarBuilder::from_mmaped_safetensors(&[dir.join(WEIGHTS)], DType::F32, &Device::Cpu)? };
        let model = marian::MTModel::new(&cfg, vb)?;
        Ok(Engine { code: lang.code, model, cfg, tok, vocab, pieces, unk })
    }

    fn sentence(&mut self, text: &str) -> Result<String> {
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        let enc = self.tok.encode(text.as_str(), false).map_err(anyhow::Error::msg)?;
        let mut ids: Vec<u32> = enc
            .get_tokens()
            .iter()
            .map(|t| self.vocab.get(t).copied().unwrap_or(self.unk))
            .collect();
        ids.truncate(self.cfg.max_position_embeddings.saturating_sub(2));
        ids.push(self.cfg.eos_token_id);
        self.model.reset_kv_cache();
        let input = Tensor::new(ids.as_slice(), &Device::Cpu)?.unsqueeze(0)?;
        let encoded = self.model.encoder().forward(&input, 0)?;
        let mut out = vec![self.cfg.decoder_start_token_id];
        let max = (ids.len() * 3 + 10).min(self.cfg.max_position_embeddings - 1);
        for step in 0..max {
            let ctx = if step == 0 { out.len() } else { 1 };
            let start = out.len() - ctx;
            let input = Tensor::new(&out[start..], &Device::Cpu)?.unsqueeze(0)?;
            let logits = self.model.decode(&input, &encoded, start)?.squeeze(0)?;
            let logits = logits.get(logits.dim(0)? - 1)?.to_vec1::<f32>()?;
            // Greedy, never the padding token.
            let next = logits
                .iter()
                .enumerate()
                .filter(|&(i, _)| i as u32 != self.cfg.pad_token_id)
                .max_by(|a, b| a.1.total_cmp(b.1))
                .map_or(self.cfg.eos_token_id, |(i, _)| i as u32);
            if next == self.cfg.eos_token_id || next == self.cfg.forced_eos_token_id {
                break;
            }
            out.push(next);
        }
        let text: String = out[1..]
            .iter()
            .filter_map(|&i| self.pieces.get(i as usize))
            .filter(|p| !(p.starts_with('<') && p.ends_with('>')))
            .map(String::as_str)
            .collect();
        Ok(text.replace('▁', " ").trim().to_string())
    }
}

/// The one loaded model (about 300 MB of memory). It loads on the first
/// translation and goes again after [`IDLE`] without one.
static ENGINE: Mutex<Option<Engine>> = Mutex::new(None);

/// Bumped by every translation that uses the model, so an unload timer
/// can tell whether the model was used since it started.
static USES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// How long the model stays in memory after its last use. Loading it
/// again takes well under a second.
const IDLE: std::time::Duration = std::time::Duration::from_secs(if cfg!(test) { 2 } else { 10 });

/// Frees the model once it has gone [`IDLE`] without a translation.
fn unload_when_idle() {
    let n = USES.load(std::sync::atomic::Ordering::SeqCst);
    std::thread::spawn(move || {
        std::thread::sleep(IDLE);
        let mut e = ENGINE.lock().unwrap();
        // Checked under the lock: a translation bumps it under the lock too.
        if USES.load(std::sync::atomic::Ordering::SeqCst) == n {
            *e = None;
        }
    });
}

/// Whether a model is in memory now.
pub fn loaded() -> bool {
    ENGINE.lock().unwrap().is_some()
}

fn unload(lang: &Lang) {
    let mut e = ENGINE.lock().unwrap();
    if e.as_ref().is_some_and(|e| e.code == lang.code) {
        *e = None;
    }
}

/// Frees the loaded model's memory.
pub fn unload_all() {
    *ENGINE.lock().unwrap() = None;
}

// ---- cache ------------------------------------------------------------------------

static CACHES: Mutex<Option<HashMap<&'static str, HashMap<String, String>>>> = Mutex::new(None);

fn cache_path(lang: &Lang) -> Option<PathBuf> {
    root().map(|r| r.join(format!("cache-{}.json", lang.code)))
}

fn with_cache<T>(lang: &'static Lang, f: impl FnOnce(&mut HashMap<String, String>) -> T) -> T {
    let mut all = CACHES.lock().unwrap();
    let all = all.get_or_insert_with(HashMap::new);
    let c = all.entry(lang.code).or_insert_with(|| {
        cache_path(lang)
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    });
    f(c)
}

/// `md` with its `lang` text in English when every sentence of it was
/// translated before, without loading the model.
pub fn cached(md: &str, lang: &'static Lang) -> Option<String> {
    with_cache(lang, |c| rebuild(md, lang, |s| c.get(s).cloned()))
}

/// Languages written without spaces between words.
fn spaceless(lang: &Lang) -> bool {
    matches!(lang.code, "zh" | "ja")
}

/// Rebuilds `md` with each piece's sentences put through `tr`; `None` when
/// `tr` has no answer for one.
fn rebuild(md: &str, lang: &Lang, mut tr: impl FnMut(&str) -> Option<String>) -> Option<String> {
    let mut out = String::with_capacity(md.len());
    let mut at = 0;
    // Whether the piece written last was translated.
    let mut last_done = false;
    for r in pieces(md) {
        let piece = &md[r.clone()];
        if !wants(lang, piece) {
            continue;
        }
        let mut parts = Vec::new();
        for s in sentences(piece) {
            parts.push(if wants(lang, s) { sentence(lang, s, &mut tr)? } else { s.to_string() });
        }
        let gap = &md[at..r.start];
        // "**新增：**一格" has no space after the bold; English needs one.
        let (before, after) = if spaceless(lang) && at > 0 { gap_spaces(md, at, gap) } else { (false, false) };
        if before && last_done {
            out.push(' ');
        }
        out.push_str(gap);
        let text = parts.join(" ");
        if after && !text.starts_with([',', '.', ':', ';', '!', '?', ')']) {
            out.push(' ');
        }
        out.push_str(&text);
        at = r.end;
        last_done = true;
    }
    out.push_str(&md[at..]);
    Some(out)
}

/// Whether a space goes before or after `gap` (markdown between two pieces
/// at `at`) when it is only emphasis marks with no space: after it when it
/// closes emphasis, before it when it opens one.
fn gap_spaces(md: &str, at: usize, gap: &str) -> (bool, bool) {
    if gap.is_empty() || !gap.chars().all(|c| matches!(c, '*' | '_')) {
        return (false, false);
    }
    let line = &md[md[..at].rfind('\n').map_or(0, |i| i + 1)..at];
    let line = &line[prefix_len(line)..];
    let runs = line
        .as_bytes()
        .split(|b| !matches!(b, b'*' | b'_'))
        .filter(|r| !r.is_empty())
        .count();
    if runs % 2 == 1 { (false, true) } else { (true, false) }
}

/// One sentence through `tr`.
fn sentence(lang: &Lang, s: &str, tr: &mut impl FnMut(&str) -> Option<String>) -> Option<String> {
    // Arrows, emoji and other symbols aren't in the model's vocabulary and
    // would be dropped: keep them, and translate the text between.
    let mut out = String::new();
    let mut run_start = 0;
    let mut flush = |out: &mut String, run: &str| -> Option<()> {
        if !wants(lang, run) {
            out.push_str(run);
            return Some(());
        }
        let t = run.trim();
        let lead = &run[..run.len() - run.trim_start().len()];
        let trail = &run[run.trim_end().len()..];
        out.push_str(lead);
        out.push_str(&tidy(lang, &words(lang, t, tr)?));
        out.push_str(trail);
        Some(())
    };
    for (i, c) in s.char_indices() {
        if symbol(c) {
            flush(&mut out, &s[run_start..i])?;
            out.push(c);
            run_start = i + c.len_utf8();
        }
    }
    flush(&mut out, &s[run_start..])?;
    Some(out)
}

/// Characters kept as they are: arrows, shapes, dingbats, emoji.
fn symbol(c: char) -> bool {
    matches!(c as u32, 0x2190..=0x21FF | 0x2300..=0x23FF | 0x2460..=0x24FF | 0x25A0..=0x27BF | 0x2900..=0x297F | 0x2B00..=0x2BFF | 0x1F000..=0x1FAFF | 0xFE0F | 0x200D)
}

/// The model's English from a spaceless language can glue words to
/// punctuation ("Name:Order"); give them their space.
fn tidy(lang: &Lang, s: &str) -> String {
    if !spaceless(lang) {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 4);
    let mut prev = None;
    for c in s.chars() {
        if matches!(prev, Some(':' | ';' | ',')) && c.is_alphabetic() {
            out.push(' ');
        }
        out.push(c);
        prev = Some(c);
    }
    out
}

/// One run of text through `tr`, leaving English words at its start or end
/// (a mod's name before "更新日志") as they are.
fn words(lang: &Lang, s: &str, tr: &mut impl FnMut(&str) -> Option<String>) -> Option<String> {
    if matches!(lang.code, "zh" | "ja" | "ko" | "ru" | "uk") {
        let script = |c: char| wants(lang, c.encode_utf8(&mut [0; 4]));
        let first = s.find(script)?;
        let last = s.char_indices().filter(|&(_, c)| script(c)).last().map_or(s.len(), |(i, c)| i + c.len_utf8());
        let (head, tail) = (&s[..first], &s[last..]);
        let keep_head = head.ends_with(char::is_whitespace) && head.chars().any(|c| c.is_ascii_alphabetic());
        let keep_tail = tail.starts_with(char::is_whitespace) && tail.chars().any(|c| c.is_ascii_alphabetic());
        let mid = &s[if keep_head { first } else { 0 }..if keep_tail { last } else { s.len() }];
        let mut out = String::new();
        if keep_head {
            out.push_str(head);
        }
        out.push_str(&tr(mid)?);
        if keep_tail {
            out.push_str(tail);
        }
        return Some(out);
    }
    tr(s)
}

/// `md` with its `lang` text translated into English. Loads the model when
/// needed (the language must be [`installed`]). Slow: run it off the UI
/// thread.
pub fn translate(md: &str, lang: &'static Lang) -> Result<String> {
    if let Some(done) = cached(md, lang) {
        return Ok(done);
    }
    let dir = model_dir(lang).filter(|_| installed(lang)).with_context(|| format!("{} isn't downloaded", lang.name))?;
    let mut guard = ENGINE.lock().unwrap();
    USES.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    if guard.as_ref().is_none_or(|e| e.code != lang.code) {
        *guard = None;
        *guard = Some(Engine::load(lang, &dir).context("loading the translation model")?);
    }
    let engine = guard.as_mut().unwrap();
    let mut fresh = Vec::new();
    let mut err = None;
    let out = rebuild(md, lang, |s| {
        if let Some(t) = with_cache(lang, |c| c.get(s).cloned()) {
            return Some(t);
        }
        match engine.sentence(s) {
            Ok(t) => {
                fresh.push((s.to_string(), t.clone()));
                Some(t)
            }
            Err(e) => {
                err = Some(e);
                None
            }
        }
    });
    drop(guard);
    unload_when_idle();
    if !fresh.is_empty() {
        with_cache(lang, |c| {
            c.extend(fresh);
            if let Some(p) = cache_path(lang) {
                let _ = std::fs::create_dir_all(p.parent().unwrap());
                if let Ok(b) = serde_json::to_vec(c) {
                    let _ = std::fs::write(p, b);
                }
            }
        });
    }
    match (out, err) {
        (Some(out), _) => Ok(out),
        (None, Some(e)) => Err(e),
        (None, None) => bail!("translation failed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(md: &str) -> Vec<&str> {
        pieces(md).into_iter().map(|r| &md[r]).collect()
    }

    #[test]
    fn pieces_skip_markdown() {
        let md = "# AE2 更新日志\n\n- **新增存货：** 一格一叠 64 张。\n- see [文档](https://x.y/a_b) and `代码`\n```\n代码块\n```\n";
        assert_eq!(texts(md), ["AE2 更新日志", "新增存货：", "一格一叠 64 张。", "see", "文档", "and"]);
    }

    #[test]
    fn rebuild_keeps_syntax() {
        let md = "## 新增\n- **新增存货：** 一格一叠。\n- English stays\n";
        let lang = by_code("zh").unwrap();
        let out = rebuild(md, lang, |s| Some(format!("<{s}>"))).unwrap();
        assert_eq!(out, "## <新增>\n- **<新增存货：>** <一格一叠。>\n- English stays\n");
    }

    #[test]
    fn detects() {
        assert_eq!(detect("## 新增\n- 新增空白样板槽的实物存货：一格一叠").map(|l| l.code), Some("zh"));
        assert_eq!(detect("Added a new block and fixed a crash when opening the GUI with an empty slot.").map(|l| l.code), None);
        assert_eq!(detect("Исправлена ошибка при открытии интерфейса с пустым слотом").map(|l| l.code), Some("ru"));
        assert_eq!(detect("新しいブロックを追加しました").map(|l| l.code), Some("ja"));
    }

    /// Downloads the Chinese model (300 MB) into HOPPER_DATA_DIR and
    /// translates a real changelog.
    #[test]
    #[ignore]
    fn real_model() {
        let zh = by_code("zh").unwrap();
        download(zh, None, |_, _| {}).unwrap();
        let md = "## AE2 Pattern Disk 0.9.4 更新日志
### 新增
- **新增空白样板槽的实物存货：**一格一叠 64 张，由网络补满，可放可取。
- **新增磁盘标记的物品提示：**标记过的样板磁盘在背包里直接看到它归哪台机器。
### 修复
- **修复批处理装配室的流体替代消耗：**按容器的实际容量扣料，不再按一份扣。
- Fixed a crash (English stays)
";
        let t = std::time::Instant::now();
        let out = translate(md, zh).unwrap();
        eprintln!("{out}
-- {:?}", t.elapsed());
        assert!(!out.chars().any(is_han), "{out}");
        assert!(out.contains("- Fixed a crash (English stays)"));
        let t = std::time::Instant::now();
        assert_eq!(cached(md, zh).as_deref(), Some(out.as_str()));
        eprintln!("cached in {:?}", t.elapsed());
        // The model goes once idle, and comes back for new text.
        assert!(loaded());
        std::thread::sleep(IDLE + std::time::Duration::from_millis(500));
        assert!(!loaded());
        let t = std::time::Instant::now();
        let again = translate("- 修复了一个崩溃。
", zh).unwrap();
        eprintln!("{again} -- reloaded in {:?}", t.elapsed());
        assert!(!again.chars().any(is_han));
    }

    #[test]
    fn sentence_split() {
        assert_eq!(sentences("一格一叠。由网络补满！ok. Done"), ["一格一叠。", "由网络补满！", "ok.", "Done"]);
    }
}
