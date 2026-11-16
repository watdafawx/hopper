//! The local channel between hopperd and its edge: no TCP port, so nothing
//! on the network can reach it.
//!
//! - Windows: a named pipe that refuses remote clients, whose security
//!   descriptor lets in only the account hopperd runs as (and SYSTEM). The
//!   edge creates it as the first instance, so another program can't hold
//!   the name first, and hopperd checks the pipe's server process is the
//!   edge its run file names before it sends the token.
//! - Linux: a Unix socket in the edge folder (0700), the socket 0600, and
//!   the peer's uid checked on both ends.
//!
//! On top of that every connection starts with the edge's token (see
//! `serve`).
//!
//! The edge doesn't check which program the client is. Only this account
//! (and SYSTEM) can open the channel at all, and every program of this
//! account can read the run file with the token anyway, so a program check
//! would keep out nobody the access list doesn't. What a client can make
//! the edge do is also limited (splices only to 127.0.0.1, see `serve`).
//! The pipe's name ends in a random part from the run file, so another
//! account can't hold the name first either.

use std::path::Path;
use tokio::io::{AsyncRead, AsyncWrite};

pub trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}
pub type Stream = Box<dyn Io>;

/// Who is on the other end, as the OS says.
#[derive(Debug, Clone, Copy, Default)]
pub struct Peer {
    pub pid: Option<u32>,
}

/// The channel's address for the edge of the panel data folder `dir`.
///
/// Windows: the pipe's name ends in `random` (new for each edge, kept in
/// the run file), so no other program can know it in advance and hold it
/// first to keep the router from starting. Linux: the socket in the edge
/// folder (or the short fallback of [`super::socket_path`]).
pub fn address(dir: &Path, random: &str) -> anyhow::Result<String> {
    #[cfg(windows)]
    {
        use sha2::Digest as _;
        let full = crate::absolute(dir).to_string_lossy().to_lowercase();
        let h = sha2::Sha256::digest(full.as_bytes());
        let hex: String = h[..8].iter().map(|b| format!("{b:02x}")).collect();
        Ok(format!(r"\\.\pipe\hopperd-edge-{hex}-{random}"))
    }
    #[cfg(unix)]
    {
        let _ = random;
        let runtime = std::env::var_os("XDG_RUNTIME_DIR").map(std::path::PathBuf::from);
        let p = super::socket_path(dir, runtime.as_deref(), unsafe { libc::geteuid() });
        if p.as_os_str().len() > 107 {
            anyhow::bail!("the router's socket path {} is too long for Linux; use a shorter data folder", p.display());
        }
        Ok(p.to_string_lossy().into_owned())
    }
}

#[cfg(windows)]
pub use win::{Listener, connect};
#[cfg(unix)]
pub use ux::{Listener, connect};

#[cfg(windows)]
mod win {
    use super::{Peer, Stream};
    use anyhow::Context as _;
    use std::os::windows::io::AsRawHandle as _;
    use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeServer, ServerOptions};
    use windows::Win32::Foundation::{CloseHandle, HANDLE, HLOCAL, LocalFree};
    use windows::Win32::Security::Authorization::{ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1};
    use windows::Win32::Security::{GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser};
    use windows::Win32::System::Pipes::{GetNamedPipeClientProcessId, GetNamedPipeServerProcessId};
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    use windows::core::{HSTRING, PWSTR};

    /// This process's account, as a SID string ("S-1-5-21-…").
    pub fn own_sid() -> anyhow::Result<String> {
        unsafe {
            let mut tok = HANDLE::default();
            OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut tok)?;
            let mut len = 0u32;
            let _ = GetTokenInformation(tok, TokenUser, None, 0, &mut len);
            let mut buf = vec![0u8; len as usize];
            let r = GetTokenInformation(tok, TokenUser, Some(buf.as_mut_ptr() as *mut _), len, &mut len);
            let _ = CloseHandle(tok);
            r?;
            let user = &*(buf.as_ptr() as *const TOKEN_USER);
            let mut s = PWSTR::null();
            ConvertSidToStringSidW(user.User.Sid, &mut s)?;
            let out = s.to_string()?;
            let _ = LocalFree(Some(HLOCAL(s.0 as *mut _)));
            Ok(out)
        }
    }

    /// The pipe's access list: SYSTEM and this account, full access; nobody
    /// else; nothing inherited from elsewhere (`P`).
    pub fn sddl() -> anyhow::Result<String> {
        Ok(format!("D:P(A;;GA;;;SY)(A;;GA;;;{})", own_sid()?))
    }

    /// A security descriptor made from [`sddl`], freed when dropped.
    pub struct Sa {
        sd: PSECURITY_DESCRIPTOR,
        attrs: SECURITY_ATTRIBUTES,
    }
    // SAFETY: the descriptor is only read (by CreateNamedPipe) and freed
    // once, in Drop; nothing else holds the pointer.
    unsafe impl Send for Sa {}
    unsafe impl Sync for Sa {}

    impl Sa {
        pub fn new() -> anyhow::Result<Box<Sa>> {
            let mut sd = PSECURITY_DESCRIPTOR::default();
            unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(&HSTRING::from(sddl()?), SDDL_REVISION_1, &mut sd, None) }
                .context("making the channel's security descriptor")?;
            let mut b = Box::new(Sa { sd, attrs: SECURITY_ATTRIBUTES::default() });
            b.attrs = SECURITY_ATTRIBUTES { nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: b.sd.0, bInheritHandle: false.into() };
            Ok(b)
        }

        fn ptr(&mut self) -> *mut std::ffi::c_void {
            &mut self.attrs as *mut SECURITY_ATTRIBUTES as *mut _
        }
    }

    impl Drop for Sa {
        fn drop(&mut self) {
            unsafe {
                let _ = LocalFree(Some(HLOCAL(self.sd.0)));
            }
        }
    }

    pub struct Listener {
        name: String,
        sa: Box<Sa>,
        next: NamedPipeServer,
    }

    fn create(name: &str, first: bool, sa: &mut Sa) -> std::io::Result<NamedPipeServer> {
        let mut o = ServerOptions::new();
        o.first_pipe_instance(first).reject_remote_clients(true);
        // SAFETY: `sa` outlives the call; CreateNamedPipeW copies what it needs.
        unsafe { o.create_with_security_attributes_raw(name, sa.ptr()) }
    }

    impl Listener {
        pub fn bind(name: &str) -> anyhow::Result<Listener> {
            let mut sa = Sa::new()?;
            // First instance: if anything holds the name already, refuse it
            // rather than share the pipe with it.
            let next = create(name, true, &mut sa).with_context(|| format!("creating {name} (is another router running?)"))?;
            Ok(Listener { name: name.to_string(), sa, next })
        }

        pub async fn accept(&mut self) -> anyhow::Result<(Stream, Peer)> {
            self.next.connect().await?;
            let fresh = create(&self.name, false, &mut self.sa)?;
            let s = std::mem::replace(&mut self.next, fresh);
            let mut pid = 0u32;
            let pid = unsafe { GetNamedPipeClientProcessId(HANDLE(s.as_raw_handle()), &mut pid) }.ok().map(|_| pid);
            Ok((Box::new(s), Peer { pid }))
        }
    }

    /// Connects to the edge's pipe, and only to the edge with pid `edge`:
    /// a pipe of the same name held by another process gets nothing.
    pub async fn connect(name: &str, edge: u32) -> anyhow::Result<Stream> {
        let started = std::time::Instant::now();
        let c = loop {
            match ClientOptions::new().open(name) {
                Ok(c) => break c,
                // All instances busy for a moment.
                Err(e) if e.raw_os_error() == Some(231) && started.elapsed() < std::time::Duration::from_secs(3) => {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
                Err(e) => return Err(e).with_context(|| format!("opening {name}")),
            }
        };
        let mut pid = 0u32;
        unsafe { GetNamedPipeServerProcessId(HANDLE(c.as_raw_handle()), &mut pid) }.context("asking who holds the pipe")?;
        if pid != edge {
            anyhow::bail!("the channel is held by process {pid}, not the router ({edge})");
        }
        Ok(Box::new(c))
    }
}

#[cfg(unix)]
mod ux {
    use super::{Peer, Stream};
    use anyhow::Context as _;
    use std::os::unix::fs::{FileTypeExt as _, PermissionsExt as _};
    use tokio::net::{UnixListener, UnixStream};

    fn me() -> u32 {
        unsafe { libc::geteuid() }
    }

    pub struct Listener(UnixListener);

    impl Listener {
        pub fn bind(path: &str) -> anyhow::Result<Listener> {
            let p = std::path::Path::new(path);
            if let Some(dir) = p.parent() {
                super::super::private_folder(dir)?;
            }
            // A socket left by an edge that ended; anything else is refused,
            // and so is a socket someone still answers on (another edge:
            // deleting it would cut that one off from every hopperd).
            if let Ok(m) = std::fs::symlink_metadata(p) {
                if !m.file_type().is_socket() {
                    anyhow::bail!("{path} is there and isn't the router's socket");
                }
                if std::os::unix::net::UnixStream::connect(p).is_ok() {
                    anyhow::bail!("another router is listening on {path}");
                }
                std::fs::remove_file(p)?;
            }
            let l = UnixListener::bind(p).with_context(|| format!("listening on {path}"))?;
            std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o600))?;
            Ok(Listener(l))
        }

        pub async fn accept(&mut self) -> anyhow::Result<(Stream, Peer)> {
            loop {
                let (s, _) = self.0.accept().await?;
                // Another account can't get through the folder, but check.
                match s.peer_cred() {
                    Ok(c) if c.uid() == me() => return Ok((Box::new(s), Peer { pid: c.pid().map(|p| p as u32) })),
                    Ok(c) => log::warn!("router: refused a channel connection from uid {}", c.uid()),
                    Err(e) => log::warn!("router: refused a channel connection ({e})"),
                }
            }
        }
    }

    pub async fn connect(path: &str, edge: u32) -> anyhow::Result<Stream> {
        let s = UnixStream::connect(path).await.with_context(|| format!("connecting to {path}"))?;
        let c = s.peer_cred()?;
        if c.uid() != me() {
            anyhow::bail!("the channel is held by uid {}, not this account", c.uid());
        }
        if c.pid().is_some_and(|p| p as u32 != edge) {
            anyhow::bail!("the channel is held by process {:?}, not the router ({edge})", c.pid());
        }
        Ok(Box::new(s))
    }
}

// Only the edge tests read the descriptor text on its own.
#[cfg(all(windows, test))]
pub use win::sddl;
