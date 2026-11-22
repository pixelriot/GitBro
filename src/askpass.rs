//! Lets git and ssh ask the user for a login although GitBro has no terminal.
//!
//! Both run the program named in `GIT_ASKPASS` / `SSH_ASKPASS` with the prompt as its only
//! argument and read the answer from its stdout. GitBro names its own binary there. Started
//! that way, it forwards the prompt over a Unix socket to the running window, which shows a
//! sign-in dialog, and prints what the user entered. A cancelled dialog exits with status 1,
//! and git gives up on the operation.
//!
//! The socket sits in a directory only this user can enter, since whoever reaches it can put
//! a password prompt in front of the user.

use std::{
    io::{Read, Write},
    os::unix::{
        fs::DirBuilderExt,
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::mpsc,
    time::Duration,
};

/// Set on git's environment; its presence turns the GitBro binary into the askpass helper.
pub const SOCKET_ENV: &str = "GITBRO_ASKPASS_SOCKET";

/// Leads a reply that carries an answer. A reply without it means the user cancelled, so an
/// empty answer (an SSH key without passphrase) stays distinct from a cancel.
const ANSWER: u8 = 1;

/// A prompt waiting for the user. Dropping it unanswered counts as a cancel.
pub struct Request {
    pub prompt: String,
    reply: mpsc::Sender<Option<String>>,
}

impl Request {
    pub fn answer(self, answer: Option<String>) {
        let _ = self.reply.send(answer);
    }
}

/// Runs the helper side and exits if git started this process as its askpass program.
pub fn run_if_helper() {
    let Some(socket) = std::env::var_os(SOCKET_ENV) else {
        return;
    };
    let mut args = std::env::args().skip(1);
    let (Some(prompt), None) = (args.next(), args.next()) else {
        return;
    };
    let code = match ask(Path::new(&socket), &prompt) {
        Ok(Some(answer)) => {
            println!("{answer}");
            0
        }
        Ok(None) => 1,
        Err(err) => {
            eprintln!("gitbro askpass: {err}");
            1
        }
    };
    std::process::exit(code);
}

/// Sends `prompt` to the window listening on `socket` and waits for the user's answer.
fn ask(socket: &Path, prompt: &str) -> std::io::Result<Option<String>> {
    let mut stream = UnixStream::connect(socket)?;
    stream.write_all(prompt.as_bytes())?;
    stream.shutdown(std::net::Shutdown::Write)?;
    let mut reply = Vec::new();
    stream.read_to_end(&mut reply)?;
    Ok(match reply.split_first() {
        Some((&ANSWER, answer)) => Some(String::from_utf8_lossy(answer).into_owned()),
        _ => None,
    })
}

/// Starts listening for prompts and returns the socket path for git's environment.
/// `on_request` runs on a background thread, once per prompt, one prompt at a time.
pub fn listen(on_request: impl Fn(Request) + Send + 'static) -> std::io::Result<PathBuf> {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .unwrap_or_else(std::env::temp_dir);
    let dir = base.join(format!("gitbro-{}", std::process::id()));
    // A leftover from an earlier process with the same id.
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::DirBuilder::new().mode(0o700).create(&dir)?;
    let socket = dir.join("askpass.sock");
    let listener = UnixListener::bind(&socket)?;

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            // Prompts are served one after another; git never asks two things at once.
            let _ = serve(stream, &on_request);
        }
    });
    Ok(socket)
}

fn serve(mut stream: UnixStream, on_request: &impl Fn(Request)) -> std::io::Result<()> {
    // A client that never finishes its prompt must not block the next one forever.
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut prompt = Vec::new();
    (&mut stream).take(16 * 1024).read_to_end(&mut prompt)?;
    let (reply, answer) = mpsc::channel();
    on_request(Request {
        prompt: String::from_utf8_lossy(&prompt).into_owned(),
        reply,
    });
    match answer.recv().ok().flatten() {
        Some(answer) => {
            stream.write_all(&[ANSWER])?;
            stream.write_all(answer.as_bytes())
        }
        None => Ok(()),
    }
}

/// What a git or ssh prompt asks for.
#[derive(Debug, PartialEq)]
pub enum Prompt {
    /// git's "Username for 'https://host'"; its password prompt follows right after.
    Username { host: String },
    /// git's "Password for 'https://user@host'".
    Password { host: String, user: Option<String> },
    /// ssh's "Are you sure you want to continue connecting (yes/no/[fingerprint])?".
    TrustHost { text: String },
    /// Anything else, e.g. an SSH key passphrase.
    Other { text: String, secret: bool },
}

impl Prompt {
    pub fn parse(prompt: &str) -> Prompt {
        let text = prompt.trim().trim_end_matches(':').trim().to_owned();
        let url = prompt
            .split('\'')
            .nth(1)
            .and_then(|u| u.split_once("://").map(|(_, rest)| rest));
        if let Some(rest) = url {
            let authority = rest.split('/').next().unwrap_or(rest);
            let (user, host) = match authority.rsplit_once('@') {
                Some((user, host)) => (Some(user.to_owned()), host.to_owned()),
                None => (None, authority.to_owned()),
            };
            if prompt.starts_with("Username for ") {
                return Prompt::Username { host };
            }
            if prompt.starts_with("Password for ") {
                return Prompt::Password { host, user };
            }
        }
        if prompt.contains("(yes/no") {
            return Prompt::TrustHost { text };
        }
        let lower = prompt.to_lowercase();
        let secret = ["pass", "token", "pin", "secret"]
            .iter()
            .any(|w| lower.contains(w));
        Prompt::Other { text, secret }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_git_and_ssh_prompts() {
        assert_eq!(
            Prompt::parse("Username for 'https://github.com': "),
            Prompt::Username {
                host: "github.com".into()
            }
        );
        assert_eq!(
            Prompt::parse("Password for 'https://me@git.example.org:8443': "),
            Prompt::Password {
                host: "git.example.org:8443".into(),
                user: Some("me".into())
            }
        );
        assert!(matches!(
            Prompt::parse("Are you sure you want to continue connecting (yes/no/[fingerprint])? "),
            Prompt::TrustHost { .. }
        ));
        assert_eq!(
            Prompt::parse("Enter passphrase for key '/home/me/.ssh/id_ed25519': "),
            Prompt::Other {
                text: "Enter passphrase for key '/home/me/.ssh/id_ed25519'".into(),
                secret: true
            }
        );
    }

    #[test]
    fn round_trip_answer_and_cancel() {
        let socket = listen(|req: Request| {
            let answer = match req.prompt.as_str() {
                "name?" => Some("octo".to_owned()),
                "empty?" => Some(String::new()),
                _ => None,
            };
            req.answer(answer);
        })
        .unwrap();
        assert_eq!(ask(&socket, "name?").unwrap(), Some("octo".into()));
        assert_eq!(ask(&socket, "empty?").unwrap(), Some(String::new()));
        assert_eq!(ask(&socket, "cancel?").unwrap(), None);
        let _ = std::fs::remove_dir_all(socket.parent().unwrap());
    }
}
