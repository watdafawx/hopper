//! Browse search's typo help: lifts results whose names are close to what
//! was typed, and offers "Did you mean X?" when the page misses it.
//!
//! The names it suggests come from the most popular projects for the
//! current filters (fetched once per filter set), a search for the start
//! of each typed word, the mods installed in any instance, and bookmarks. The matching itself is
//! `hopper::namematch`.

use super::BrowseView;
use crate::widgets;
use crate::widgets::Clickable as _;
use gpui_kit::component::{ActiveTheme as _, h_flex};
use gpui_kit::*;
use hopper::cf::{self, SortField};
use hopper::namematch;
use std::collections::{HashMap, HashSet};

/// Site, class, game version and loader, then the search text: one name
/// list each. An empty text is the popular list.
type ListKey = (bool, u32, String, String, String);

/// How many pages of popular projects to learn names from.
const POPULAR_PAGES: u32 = 3;

#[derive(Default)]
pub(super) struct Suggest {
    /// Names and icons from the popular list and from shortened searches
    /// ([`namematch::probe`]), most popular first.
    lists: HashMap<ListKey, Vec<(String, String)>>,
    loading: HashSet<ListKey>,
    /// The suggestion on show: name and icon url.
    shown: Option<(String, String)>,
}

impl BrowseView {
    /// After a new first page arrives: lift close names (only for the
    /// "best first" sorts; a chosen order like Name or Updated stays) and
    /// work out a suggestion.
    pub(super) fn fuzzy_results(&mut self, cx: &mut Context<Self>) {
        if matches!(self.sort, SortField::Popularity | SortField::Featured | SortField::TotalDownloads) {
            namematch::rerank(&self.text, &mut self.results, |m| (m.name.as_str(), m.slug.as_str()));
        }
        self.refresh_suggestion(cx);
    }

    fn list_key(&self, text: &str) -> ListKey {
        let s = self.search_params();
        (self.modrinth, s.class_id, s.game_version, format!("{:?}", s.loader), text.to_string())
    }

    fn refresh_suggestion(&mut self, cx: &mut Context<Self>) {
        self.suggest.shown = None;
        let weak = !self.bookmarked
            && namematch::is_weak(&self.text, self.results.iter().map(|m| (m.name.as_str(), m.slug.as_str())));
        if !weak {
            return;
        }
        // The popular list, then a search for the start of each word, which
        // still finds a mod whose name ends in the typo.
        let keys: Vec<ListKey> = std::iter::once(String::new())
            .chain(namematch::probe(&self.text))
            .map(|t| self.list_key(&t))
            .collect();
        for k in &keys {
            if !self.suggest.lists.contains_key(k) {
                self.want_names(k.clone(), cx);
            }
        }
        // Most popular first, so ties go to the project people mean most.
        let mut names: Vec<(&str, &str)> = Vec::new();
        for k in &keys {
            if let Some(p) = self.suggest.lists.get(k) {
                names.extend(p.iter().map(|(n, i)| (n.as_str(), i.as_str())));
            }
        }
        names.extend(self.results.iter().map(|m| (m.name.as_str(), m.icon().unwrap_or(""))));
        let class = self.class;
        let store = self.store.read(cx);
        let mut seen = HashSet::new();
        let installed: Vec<(String, String)> = store
            .instances()
            .iter()
            .flat_map(|i| &i.addons)
            .filter(|a| a.class_id == class || (a.class_id == 0 && class == cf::CLASS_MODS))
            .filter(|a| seen.insert(a.name.to_lowercase()))
            .map(|a| (a.name.clone(), a.icon.clone()))
            .collect();
        names.extend(installed.iter().map(|(n, i)| (n.as_str(), i.as_str())));
        let saved: Vec<(String, String)> = hopper::bookmarks::with(|b| {
            b.list().iter().filter(|b| b.class == class).map(|b| (b.name.clone(), b.icon.clone())).collect()
        });
        names.extend(saved.iter().map(|(n, i)| (n.as_str(), i.as_str())));
        let pick = namematch::suggest(&self.text, names.iter().map(|(n, _)| *n))
            .and_then(|p| names.iter().find(|(n, _)| std::ptr::eq(*n, p)).copied())
            .map(|(n, i)| (n.to_string(), i.to_string()));
        if let Some((_, icon)) = &pick
            && !icon.is_empty()
        {
            let icon = icon.clone();
            self.store.update(cx, |s, _| s.want_image(&icon));
        }
        self.suggest.shown = pick;
    }

    /// Fetches the names for `key` once (several pages for the popular
    /// list, one for a search), then looks for a suggestion again.
    fn want_names(&mut self, key: ListKey, cx: &mut Context<Self>) {
        if !self.suggest.loading.insert(key.clone()) {
            return;
        }
        let pages = if key.4.is_empty() { POPULAR_PAGES } else { 1 };
        let base = cf::Search { text: key.4.clone(), sort: SortField::Popularity, category: None, ..self.search_params() };
        let modrinth = self.modrinth;
        cx.spawn(async move |this, cx| {
            let list = cx
                .background_spawn(async move {
                    let mut out = Vec::new();
                    for page in 0..pages {
                        let s = cf::Search { index: page * cf::PAGE_SIZE, ..base.clone() };
                        match hopper::source::search(&s, modrinth) {
                            Ok((mods, _)) if !mods.is_empty() => out.extend(
                                mods.into_iter().map(|m| (m.name.clone(), m.icon().unwrap_or("").to_string())),
                            ),
                            _ => break,
                        }
                    }
                    out
                })
                .await;
            this.update(cx, |this, cx| {
                this.suggest.loading.remove(&key);
                this.suggest.lists.insert(key, list);
                if this.suggest.shown.is_none() && !this.loading {
                    this.refresh_suggestion(cx);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// "Did you mean [icon] X?" under the search box; clicking the name
    /// searches for it.
    pub(super) fn did_you_mean_row(&self, searching: bool, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (name, icon) = self.suggest.shown.clone().filter(|_| searching && !self.loading)?;
        let icon = (!icon.is_empty()).then(|| self.store.read(cx).image(&icon).map(|p| p.to_path_buf())).flatten();
        let accent = cx.theme().primary;
        let query = name.clone();
        Some(
            h_flex()
                .gap_1()
                .text_sm()
                .child(widgets::caption("Did you mean", cx))
                .child(
                    h_flex()
                        .child(
                            widgets::mod_name("browse-did-you-mean", name, icon.as_deref(), cx)
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(accent)
                                .clickable(cx)
                                .hover(|s| s.underline())
                                .on_click(cx.listener(move |this, _, window, cx| this.set_query(&query, window, cx))),
                        )
                        .child(widgets::caption("?", cx)),
                )
                .into_any_element(),
        )
    }
}
