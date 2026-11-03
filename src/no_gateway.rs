//! The build without the Pro part has no AI gateway: `iai.exe mcp`,
//! `iai.exe run` and the relay Antigravity starts beside it
//! (`iai.exe antigravity-relay`) are the Pro pack's. Asked for one, this
//! build says so in a line and ends, rather than opening its window to an
//! agent or a script that waits for an answer on stdout.

/// The command line words the Pro pack's gateway answers.
const GATEWAY_WORDS: [&str; 3] = ["mcp", "run", "antigravity-relay"];

/// What this build says when asked for the gateway.
pub const NOT_IN_THIS_BUILD: &str = "Cổng kết nối AI có ở gói Pro";

/// The exit code: the gateway was asked for and is not here.
const EXIT_CODE: i32 = 2;

/// When the command line asks for the gateway: say it is the Pro pack's,
/// and the exit code; `None` to start the window.
pub fn refuse_if_asked() -> Option<i32> {
    let first = std::env::args().nth(1);
    let code = refusal(first.as_deref())?;
    eprintln!("{NOT_IN_THIS_BUILD}");
    Some(code)
}

/// The exit code for a command line whose first word is `first`, when that
/// word asks for the gateway.
fn refusal(first: Option<&str>) -> Option<i32> {
    first
        .filter(|word| GATEWAY_WORDS.contains(word))
        .map(|_| EXIT_CODE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_gateway_s_words_are_refused_and_the_window_starts_otherwise() {
        for word in ["mcp", "run", "antigravity-relay"] {
            let code = refusal(Some(word));
            assert!(code.is_some_and(|c| c != 0), "{word}: {code:?}");
        }
        assert_eq!(refusal(None), None);
        // A photo to open, or another of the exe's own words.
        assert_eq!(refusal(Some(r"C:\anh\the.jpg")), None);
        assert_eq!(refusal(Some("--build-info")), None);
        assert_eq!(refusal(Some("MCP")), None);
    }

    #[test]
    fn the_line_says_where_the_gateway_is_in_one_line() {
        assert_eq!(NOT_IN_THIS_BUILD, "Cổng kết nối AI có ở gói Pro");
        assert!(!NOT_IN_THIS_BUILD.contains('\n'));
    }
}
