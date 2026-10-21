//! Secret Service storage, following Git's dedicated secret-tool credentials.
use nixe_media_model::ProviderError;
use nixe_media_model::Secret;
use std::{
    io::Write,
    process::{Child, Command, Output, Stdio},
    time::{Duration, Instant},
};
type Result<T> = std::result::Result<T, ProviderError>;
fn failure() -> ProviderError {
    ProviderError::Failed("Unlock the desktop keyring and make sure libsecret is installed".into())
}
fn tool(verb: &str, provider: &str, address: &str) -> Command {
    let mut command = Command::new("secret-tool");
    command.arg(verb);
    if verb == "store" {
        command.arg(format!("--label=Theater {provider} server"));
    }
    command.args(["application", "nixe-theater", "provider", provider, "server", address]).stderr(Stdio::null());
    command
}
fn wait_output(mut child: Child, limit: Duration) -> Result<Output> {
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return child.wait_with_output().map_err(|_| failure()),
            Ok(None) if started.elapsed() < limit => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                child.stdout.take();
                child.stderr.take();
                let _ = child.wait();
                return Err(ProviderError::Failed(
                    "The desktop keyring did not respond. Unlock it, then retry the connection.".into(),
                ));
            }
        }
    }
}
pub fn save(address: &str, secret: &Secret) -> Result<()> {
    save_for("plex", address, secret)
}
pub fn save_for(provider: &str, address: &str, secret: &Secret) -> Result<()> {
    let mut child =
        tool("store", provider, address).stdin(Stdio::piped()).stdout(Stdio::null()).spawn().map_err(|_| failure())?;
    let mut input = child.stdin.take().ok_or_else(failure)?;
    let write = input.write_all(secret.expose().as_bytes());
    drop(input);
    let status = wait_output(child, Duration::from_secs(60))?.status;
    write.map_err(|_| failure())?;
    if status.success() { Ok(()) } else { Err(failure()) }
}
pub fn load(address: &str) -> Result<Secret> {
    load_for("plex", address)
}
pub fn load_for(provider: &str, address: &str) -> Result<Secret> {
    let child = tool("lookup", provider, address).stdin(Stdio::null()).stdout(Stdio::piped()).spawn().map_err(|_| failure())?;
    let output = wait_output(child, Duration::from_secs(8))?;
    if !output.status.success() {
        return Err(failure());
    }
    let value = String::from_utf8(output.stdout).map_err(|_| failure())?;
    Secret::new(value.trim_end_matches('\n').into())
}
pub fn delete(address: &str) -> Result<()> {
    delete_for("plex", address)
}
pub fn delete_for(provider: &str, address: &str) -> Result<()> {
    let child = tool("clear", provider, address).stdin(Stdio::null()).stdout(Stdio::null()).spawn().map_err(|_| failure())?;
    let status = wait_output(child, Duration::from_secs(8))?.status;
    if status.success() { Ok(()) } else { Err(failure()) }
}

/// The Plex account token from signing in, kept so a server that moved can be found again.
const ACCOUNT: (&str, &str) = ("plex-account", "plex.tv");
pub fn save_account(secret: &Secret) -> Result<()> {
    save_for(ACCOUNT.0, ACCOUNT.1, secret)
}
pub fn load_account() -> Result<Secret> {
    load_for(ACCOUNT.0, ACCOUNT.1)
}
pub fn delete_account() -> Result<()> {
    delete_for(ACCOUNT.0, ACCOUNT.1)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_stalled_credential_process_is_reaped() {
        let child = Command::new("sleep").arg("10").stdout(Stdio::piped()).spawn().unwrap();
        let start = Instant::now();
        assert!(wait_output(child, Duration::from_millis(30)).is_err());
        assert!(start.elapsed() < Duration::from_secs(1));
    }
}
