//! The user this process runs as, which names the launchd domain the agent is loaded into.
//!
//! `launchctl` addresses a per-user agent as `gui/<uid>/<label>`, so the service layer needs the
//! user id. The call itself is a libc one, and every call that needs `unsafe` lives here.

/// Returns the user id this process runs as.
pub fn uid() -> u32 {
    unsafe { libc::getuid() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_user_does_not_change_between_calls() {
        assert_eq!(uid(), uid());
    }
}
