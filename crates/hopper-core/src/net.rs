//! Opening connections to web hosts on networks that lose some of them.
//!
//! On some routes a share of new connections to a host never get an
//! answer: the first packet and every resend of it vanish, while the next
//! connection (from a new local port) goes through at once. Seen on
//! Modrinth's CDN: a quarter to almost half of the connections to two of
//! its four addresses went unanswered, the others never. Windows keeps
//! resending into the dead one for about 21 seconds, so an icon or a
//! download waited that long (or until the agent's connect limit) and then
//! failed. Here a connection that hasn't answered in a second or two is
//! dropped and a fresh one opened, within the agent's own connect limit.

use std::time::{Duration, Instant};
use ureq::unversioned::transport::{ConnectionDetails, Connector, DefaultConnector, NextTimeout, Transport};

/// How long each try may wait for the host to answer before a fresh
/// connection is opened. A healthy connect takes well under a second (one
/// lost packet is resent after 1 s), and Windows takes about 2 s to report
/// a refused one, so the first try is 2.5 s: a refused host still fails
/// at once rather than being tried again. Later tries get longer, for slow
/// links. After these, a try gets all the time left.
const TRIES: [Duration; 4] =
    [Duration::from_millis(2500), Duration::from_secs(3), Duration::from_secs(4), Duration::from_secs(6)];

/// An agent with `config` whose connects retry as described above.
pub fn agent(config: ureq::config::Config) -> ureq::Agent {
    ureq::Agent::with_parts(config, Retry(DefaultConnector::default()), ureq::unversioned::resolver::DefaultResolver::default())
}

/// Wraps a connector: a try that times out connecting is tried again on a
/// new connection while the connect limit allows.
#[derive(Debug)]
struct Retry<C>(C);

impl<C: Connector<(), Out = Box<dyn Transport>>> Connector<()> for Retry<C> {
    type Out = Box<dyn Transport>;

    fn connect(&self, d: &ConnectionDetails, chained: Option<()>) -> Result<Option<Self::Out>, ureq::Error> {
        // `None`: the agent sets no connect limit.
        let limit = d.timeout.not_zero().map(|t| *t);
        let began = Instant::now();
        for n in 0.. {
            let left = limit.map(|l| l.saturating_sub(began.elapsed()));
            let wait = TRIES.get(n).copied();
            // The last try: the default chain with whatever time is left.
            let this = match (wait, left) {
                (Some(w), Some(l)) if w < l => w,
                (Some(w), None) => w,
                _ => return self.0.connect(&with_timeout(d, left, d.timeout), chained),
            };
            match self.0.connect(&with_timeout(d, Some(this), d.timeout), chained) {
                Err(ureq::Error::Timeout(_)) => {
                    log::debug!("connecting to {} took over {this:?}; trying a new connection", d.uri);
                }
                other => return other,
            }
        }
        unreachable!()
    }
}

/// `d` with `after` (or no limit) as its connect timeout.
fn with_timeout<'a>(d: &ConnectionDetails<'a>, after: Option<Duration>, was: NextTimeout) -> ConnectionDetails<'a> {
    ConnectionDetails {
        uri: d.uri,
        addrs: d.addrs.clone(),
        config: d.config,
        request_level: d.request_level,
        resolver: d.resolver,
        now: (d.current_time)(),
        timeout: NextTimeout {
            after: after.map_or(ureq::unversioned::transport::time::Duration::NotHappening, Into::into),
            reason: was.reason,
        },
        current_time: d.current_time.clone(),
        run_connector: d.run_connector.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// A connector whose first `stall` tries time out (after a short
    /// sleep, standing in for the wait), then hands over to the real one.
    #[derive(Debug)]
    struct Flaky {
        stall: usize,
        tries: Arc<Mutex<Vec<Option<Duration>>>>,
    }

    impl Connector<()> for Flaky {
        type Out = Box<dyn Transport>;
        fn connect(&self, d: &ConnectionDetails, chained: Option<()>) -> Result<Option<Self::Out>, ureq::Error> {
            let mut tries = self.tries.lock().unwrap();
            tries.push(d.timeout.not_zero().map(|t| *t));
            if tries.len() <= self.stall {
                return Err(ureq::Error::Timeout(ureq::Timeout::Connect));
            }
            drop(tries);
            DefaultConnector::default().connect(d, chained)
        }
    }

    fn serve_once() -> String {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let addr = server.server_addr().to_ip().unwrap();
        std::thread::spawn(move || {
            if let Ok(rq) = server.recv() {
                let _ = rq.respond(tiny_http::Response::from_string("icon"));
            }
        });
        format!("http://{addr}/icon.webp")
    }

    fn agent_with(stall: usize, connect: Option<u64>) -> (ureq::Agent, Arc<Mutex<Vec<Option<Duration>>>>) {
        let tries = Arc::new(Mutex::new(Vec::new()));
        let config = ureq::Agent::config_builder().timeout_connect(connect.map(Duration::from_secs)).build();
        let agent = ureq::Agent::with_parts(
            config,
            Retry(Flaky { stall, tries: tries.clone() }),
            ureq::unversioned::resolver::DefaultResolver::default(),
        );
        (agent, tries)
    }

    #[test]
    fn a_connect_that_hangs_is_tried_again_on_a_new_connection() {
        let url = serve_once();
        let (agent, tries) = agent_with(2, Some(30));
        let body = agent.get(&url).call().unwrap().into_body().read_to_string().unwrap();
        assert_eq!(body, "icon");
        let tries = tries.lock().unwrap();
        assert_eq!(tries.len(), 3);
        // Short tries first, then a longer one.
        assert_eq!(tries[0], Some(TRIES[0]));
        assert_eq!(tries[1], Some(TRIES[1]));
        assert_eq!(tries[2], Some(TRIES[2]));
    }

    #[test]
    fn the_tries_stay_inside_the_connect_limit() {
        let url = serve_once();
        // A 2.8 s limit: one 2.5 s try, then the last one gets what's
        // left (a 3 s try would overrun it).
        let tries = {
            let tries = Arc::new(Mutex::new(Vec::new()));
            let config = ureq::Agent::config_builder().timeout_connect(Some(Duration::from_millis(2800))).build();
            let flaky = Retry(Flaky { stall: 1, tries: tries.clone() });
            let agent = ureq::Agent::with_parts(config, flaky, ureq::unversioned::resolver::DefaultResolver::default());
            agent.get(&url).call().unwrap();
            tries
        };
        let tries = tries.lock().unwrap();
        assert_eq!(tries.len(), 2);
        assert_eq!(tries[0], Some(TRIES[0]));
        let last = tries[1].unwrap();
        assert!(last <= Duration::from_millis(2800) && last > TRIES[0], "{last:?}");
    }

    #[test]
    fn a_host_that_never_answers_still_fails() {
        let (agent, tries) = agent_with(usize::MAX, Some(20));
        let err = agent.get("http://127.0.0.1:9/x").call().unwrap_err();
        assert!(matches!(err, ureq::Error::Timeout(_)), "{err}");
        // Every short try, then one with the rest of the limit.
        assert_eq!(tries.lock().unwrap().len(), TRIES.len() + 1);
    }

    #[test]
    fn other_errors_are_not_retried() {
        // Nothing listens on this port: refused at once, no second try.
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let (agent, tries) = agent_with(0, Some(10));
        assert!(agent.get(&format!("http://127.0.0.1:{port}/x")).call().is_err());
        assert_eq!(tries.lock().unwrap().len(), 1);
    }
}
