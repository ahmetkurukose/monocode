//! A project's running dev servers, for the new tab page (context.md K66):
//! the local ports its runs and terminals listen on.
//!
//! A listening port belongs to the project when the process holding it is
//! one of the project's runs or terminal shells, or descends from one.
//! A run that announced its address (`code::run::ports`) counts even when
//! its process tree cannot be followed (a container, a daemon).

use std::collections::{HashMap, HashSet};

/// A process the project started: a run, or a terminal's shell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Root {
    pub pid: u32,
    /// What the user knows it as: the run's label, or "Terminal".
    pub name: String,
    /// The address the run announced, if it did.
    pub url: Option<String>,
}

/// One server to show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DevServer {
    pub name: String,
    pub port: u16,
    pub url: String,
    /// The listening program, as `lsof` names it (truncated); empty for an
    /// announced address no process of ours was seen on.
    pub command: String,
}

/// The servers `roots` run, given the machine's listening ports (port →
/// pid, command) and its process tree (pid → parent pid). Sorted by port.
pub fn find(
    roots: &[Root],
    listeners: &HashMap<u16, (u32, String)>,
    parents: &HashMap<u32, u32>,
) -> Vec<DevServer> {
    let by_pid: HashMap<u32, &Root> = roots.iter().map(|r| (r.pid, r)).collect();
    let mut found: HashMap<u16, DevServer> = HashMap::new();
    for (&port, (pid, command)) in listeners {
        let Some(root) = ancestor_root(*pid, &by_pid, parents) else {
            continue;
        };
        let url = root
            .url
            .clone()
            .filter(|url| url_port(url) == Some(port))
            .unwrap_or_else(|| format!("http://localhost:{port}/"));
        found.insert(
            port,
            DevServer {
                name: root.name.clone(),
                port,
                url,
                command: command.clone(),
            },
        );
    }
    // An announced address whose port is listened on by someone else is
    // not ours; one nobody seems to listen on may be behind a container.
    for root in roots {
        let Some(url) = &root.url else { continue };
        let Some(port) = url_port(url) else { continue };
        if found.contains_key(&port) || listeners.contains_key(&port) {
            continue;
        }
        found.insert(
            port,
            DevServer {
                name: root.name.clone(),
                port,
                url: url.clone(),
                command: String::new(),
            },
        );
    }
    let mut servers: Vec<DevServer> = found.into_values().collect();
    servers.sort_by_key(|s| s.port);
    servers
}

/// The root `pid` is, or descends from.
fn ancestor_root<'a>(
    pid: u32,
    roots: &HashMap<u32, &'a Root>,
    parents: &HashMap<u32, u32>,
) -> Option<&'a Root> {
    let mut seen = HashSet::new();
    let mut at = pid;
    while at > 1 && seen.insert(at) {
        if let Some(root) = roots.get(&at) {
            return Some(root);
        }
        at = *parents.get(&at)?;
    }
    None
}

/// The port of an http(s) URL, explicit or the scheme's.
pub fn url_port(url: &str) -> Option<u16> {
    let parsed = url::Url::parse(url).ok()?;
    parsed.port_or_known_default()
}

/// The machine's listening TCP ports now (one `lsof`).
#[cfg(unix)]
pub fn listeners() -> HashMap<u16, (u32, String)> {
    crate::db::tunnel::listeners()
        .into_iter()
        .map(|(port, l)| (port, (l.pid, l.command)))
        .collect()
}

#[cfg(not(unix))]
pub fn listeners() -> HashMap<u16, (u32, String)> {
    HashMap::new()
}

/// The machine's process tree now: pid → parent pid (one `ps`).
#[cfg(unix)]
pub fn parents() -> HashMap<u32, u32> {
    let out = std::process::Command::new("/bin/ps")
        .args(["-axo", "pid=,ppid="])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output();
    out.map(|out| parse_ps(&String::from_utf8_lossy(&out.stdout)))
        .unwrap_or_default()
}

#[cfg(not(unix))]
pub fn parents() -> HashMap<u32, u32> {
    HashMap::new()
}

fn parse_ps(text: &str) -> HashMap<u32, u32> {
    text.lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let pid = parts.next()?.parse().ok()?;
            let ppid = parts.next()?.parse().ok()?;
            Some((pid, ppid))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(pid: u32, name: &str, url: Option<&str>) -> Root {
        Root {
            pid,
            name: name.into(),
            url: url.map(Into::into),
        }
    }

    #[test]
    fn ports_held_by_a_projects_processes_or_their_children_are_its_servers() {
        let roots = [
            root(100, "Terminal", None),
            root(200, "dev", Some("http://localhost:5173/app")),
        ];
        // 100 → 110 (npm) → 120 (node); 200 → 210 (vite); 300 is not ours.
        let parents = HashMap::from([(110, 100), (120, 110), (210, 200), (300, 1), (100, 50)]);
        let listeners = HashMap::from([
            (3000, (120, "node".to_owned())),
            (5173, (210, "node".to_owned())),
            (8080, (300, "java".to_owned())),
        ]);
        let found = find(&roots, &listeners, &parents);
        assert_eq!(
            found,
            [
                DevServer {
                    name: "Terminal".into(),
                    port: 3000,
                    url: "http://localhost:3000/".into(),
                    command: "node".into(),
                },
                DevServer {
                    name: "dev".into(),
                    port: 5173,
                    // The address the run announced, path and all.
                    url: "http://localhost:5173/app".into(),
                    command: "node".into(),
                },
            ]
        );
    }

    #[test]
    fn an_announced_address_counts_unless_someone_elses_process_holds_it() {
        let roots = [
            root(200, "docker", Some("http://127.0.0.1:8000/")),
            root(300, "api", Some("http://localhost:9000/")),
        ];
        let listeners = HashMap::from([(9000, (999, "other".to_owned()))]);
        let found = find(&roots, &listeners, &HashMap::new());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].port, 8000);
        assert_eq!(found[0].url, "http://127.0.0.1:8000/");
        assert!(found[0].command.is_empty());
    }

    #[test]
    fn a_loop_in_the_process_tree_ends() {
        let roots = [root(100, "Terminal", None)];
        let parents = HashMap::from([(5, 6), (6, 5)]);
        let listeners = HashMap::from([(3000, (5, "node".to_owned()))]);
        assert!(find(&roots, &listeners, &parents).is_empty());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_port_this_process_listens_on_is_found_on_the_machine() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let me = std::process::id();
        let parents = parents();
        assert!(parents.contains_key(&me), "ps lists this process");
        let roots = [root(me, "Test", None)];
        let found = find(&roots, &listeners(), &parents);
        assert!(
            found
                .iter()
                .any(|s| s.port == port && s.url == format!("http://localhost:{port}/")),
            "{found:?}"
        );
    }

    #[test]
    fn ps_output_reads_as_pids_and_parents() {
        let parsed = parse_ps("    1     0\n  412     1\nbad line\n 9000   412\n");
        assert_eq!(parsed, HashMap::from([(1, 0), (412, 1), (9000, 412)]));
        assert_eq!(url_port("http://localhost:3000/x"), Some(3000));
        assert_eq!(url_port("https://localhost/"), Some(443));
        assert_eq!(url_port("nonsense"), None);
    }
}
