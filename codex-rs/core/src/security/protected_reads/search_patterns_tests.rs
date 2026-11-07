use super::search_pattern_indices;
use pretty_assertions::assert_eq;

/// The pattern words of `script` run by `bash -c`, split as
/// `command_words` splits it.
fn patterns(script: &str) -> Vec<String> {
    let command = [
        "/bin/bash".to_string(),
        "-c".to_string(),
        script.to_string(),
    ];
    let words = super::super::command_words(&command, /*windows*/ false);
    let mut indices: Vec<usize> = search_pattern_indices(&command, &words, /*windows*/ false)
        .into_iter()
        .collect();
    indices.sort_unstable();
    indices
        .into_iter()
        .map(|index| words[index].clone())
        .collect()
}

#[test]
fn leading_assignments_before_searches_are_program_prefixes() {
    for script in [
        "LC_ALL=C grep .env logs",
        "LC_ALL=C LANG=en_US rg .env logs",
        "EMPTY= _MODE2=dev egrep .env logs",
        "MODE=a=b fgrep .env logs",
        "LC_ALL=C /usr/bin/grep .env logs",
        "LC_ALL=C git grep .env -- logs",
        "env LC_ALL=C grep .env logs",
        "LC_ALL=C command grep .env logs",
        "sudo LC_ALL=C grep .env logs",
        "time LC_ALL=C grep .env logs",
    ] {
        assert_eq!(patterns(script), [".env"], "{script}");
    }
}

#[test]
fn assignment_like_arguments_do_not_start_searches() {
    for script in [
        "cat LC_ALL=C grep .env",
        "echo LC_ALL=C LANG=C rg .env",
        "cat env LC_ALL=C grep .env",
        "=C grep .env logs",
        "1MODE=C grep .env logs",
        "MODE-NAME=C grep .env logs",
        "./MODE=C grep .env logs",
        "MODE[0]=C grep .env logs",
        "MODE+=C grep .env logs",
        // These prefixes run the next word as the program: `X=1` here.
        "nohup X=1 grep .env logs",
        "command X=1 grep .env logs",
        "exec X=1 grep .env logs",
    ] {
        assert_eq!(patterns(script), Vec::<String>::new(), "{script}");
    }
}

#[test]
fn assigned_searches_keep_actual_paths_out_of_patterns() {
    for script in [
        "LC_ALL=C grep token .env",
        "LC_ALL=C rg -e token .env",
        "MODE=.npmrc grep token .env",
    ] {
        assert_eq!(patterns(script), ["token"], "{script}");
    }
    assert_eq!(
        patterns("LC_ALL=C grep -f patterns.txt .env"),
        Vec::<String>::new()
    );
    assert_eq!(patterns("LC_ALL=C rg --files .env"), Vec::<String>::new());
}

#[test]
fn assignment_prefixes_respect_command_boundaries() {
    for script in [
        "cat logs; LC_ALL=C grep .env logs",
        "cat logs | LC_ALL=C rg .env",
        "echo done && LC_ALL=C grep .env logs",
        "cat MODE=C; grep .env logs",
        "echo done\nLC_ALL=C grep .env logs",
    ] {
        assert_eq!(patterns(script), [".env"], "{script}");
    }
    assert_eq!(patterns("LC_ALL=C\ncat grep .env"), Vec::<String>::new());
    assert_eq!(patterns("LC_ALL=C grep -e token; cat .env"), ["token;"]);
}

#[test]
fn the_first_operand_of_a_search_is_its_pattern() {
    assert_eq!(patterns(r#"rg ".env" logs/"#), [".env"]);
    assert_eq!(patterns("rg -n -i .env logs/"), [".env"]);
    assert_eq!(patterns("grep -rn .env logs/ 2>/dev/null"), [".env"]);
    assert_eq!(patterns("grep -A 3 -m 1 .env logs/x.log"), [".env"]);
    assert_eq!(patterns("rg -g '*.log' --max-depth 2 .env logs/"), [".env"]);
    assert_eq!(patterns("rg -n -- .env logs/"), [".env"]);
    assert_eq!(patterns("cat logs/x.log | grep .env"), [".env"]);
    assert_eq!(patterns("cd logs && egrep .env x.log; echo done"), [".env"]);
    assert_eq!(patterns("git grep -n .env -- logs/"), [".env"]);
    assert_eq!(patterns("/usr/bin/grep .env x.log"), [".env"]);
}

#[test]
fn quoted_or_escaped_suffixes_do_not_start_searches() {
    for (script, pattern) in [
        ("grep 'a|' rg .env", "a|"),
        ("grep \"a|\" grep .env", "a|"),
        ("grep a\\| egrep .env", "a|"),
        ("grep 'a;' fgrep .env", "a;"),
        ("grep \"a&\" rg .env", "a&"),
        ("grep a\\& rg .env", "a&"),
    ] {
        assert_eq!(patterns(script), [pattern], "{script}");
    }
}

#[test]
fn literal_separator_words_are_patterns_and_option_values() {
    for (script, pattern) in [
        ("grep '|' rg .env", "|"),
        ("grep ';' rg .env", ";"),
        ("grep '(' rg .env", "("),
        ("grep ')' rg .env", ")"),
        ("grep '!' rg .env", "!"),
        ("grep -e '|' rg .env", "|"),
        ("grep --regexp='a|' rg .env", "--regexp=a|"),
        ("rg --glob 'a|' token .env", "token"),
    ] {
        assert_eq!(patterns(script), [pattern], "{script}");
    }
    assert_eq!(patterns("grep -f 'a|' rg .env"), Vec::<String>::new());
}

#[test]
fn real_separators_after_quoted_patterns_still_end_searches() {
    for script in [
        "grep 'a|' logs; rg .env logs",
        "grep 'a'| rg .env logs",
        "grep 'a|' logs && rg .env logs",
        "grep 'a|' logs\ngrep .env logs",
    ] {
        assert_eq!(patterns(script), ["a|", ".env"], "{script}");
    }
    for script in ["grep 'a|'; rg .env logs", "grep a\\|; rg .env logs"] {
        assert_eq!(patterns(script), ["a|;", ".env"], "{script}");
    }
}

#[test]
fn separator_provenance_survives_empty_quotes_continuations_and_comments() {
    for script in ["grep 'a|'\"\" rg .env", "grep a\\\n\\| rg .env"] {
        assert_eq!(patterns(script), ["a|"], "{script}");
    }
    assert_eq!(patterns("grep 'a|b' .env"), ["a|b"]);
    assert_eq!(patterns("grep 'a\nb|' rg .env"), ["a\nb|"]);
    assert_eq!(patterns("# don't split '|\ngrep .env logs"), [".env"]);
}

#[test]
fn carriage_returns_stay_in_words_as_shlex_keeps_them() {
    // Shlex and bash do not split at `\r`; a CRLF script still has patterns.
    assert_eq!(patterns("grep .env x.log\r"), [".env"]);
    assert_eq!(patterns("grep 'a|' rg .env\r\n"), ["a|"]);
}

#[test]
fn a_pattern_option_gives_the_patterns() {
    assert_eq!(
        patterns("grep -e .env -e .npmrc logs/x.log"),
        [".env", ".npmrc"]
    );
    assert_eq!(patterns("grep -ie .env logs/x.log"), [".env"]);
    assert_eq!(patterns("rg --regexp .env logs/"), [".env"]);
    assert_eq!(patterns("rg --regexp=.env logs/"), ["--regexp=.env"]);
    // The patterns come from a file: every operand is a path.
    assert!(patterns("grep -f patterns.txt .env").is_empty());
    assert!(patterns("rg --file=patterns.txt .env").is_empty());
    // `-e` after the operands (GNU grep permutes).
    assert_eq!(patterns("grep secret .env -e token"), ["token"]);
}

/// The paths a search reads stay paths: no false negative.
#[test]
fn searched_paths_are_not_patterns() {
    assert_eq!(patterns("grep secret .env; echo \"exit=$?\""), ["secret"]);
    assert_eq!(patterns("rg -n token ~/.npmrc"), ["token"]);
    assert!(patterns("rg --files .env").is_empty());
    assert!(patterns("cat .env; echo \"exit=$?\"").is_empty());
    assert!(patterns("echo grep .env").is_empty());
    // An option this does not know is taken as a flag: at worst its value
    // is taken for the pattern, never a path after the pattern.
    assert_eq!(patterns("grep --unknown 3 secret .env"), ["3"]);
    // A count is taken only when it is one (BSD `grep -C` has an optional
    // count).
    assert_eq!(patterns("grep -C token .env"), ["token"]);
    // BSD grep's `--context` takes an optional count.
    assert_eq!(patterns("grep --context 2 .env"), ["2"]);
    assert_eq!(patterns("grep &>> log token .env"), ["token"]);
    assert_eq!(patterns("grep 2>&1 token .env"), ["token"]);
    // GNU grep's abbreviations of `--regexp`.
    assert_eq!(patterns("grep --reg=token .env"), ["--reg=token"]);
    assert_eq!(patterns("grep --rege token .env"), ["token"]);
    // A pattern that runs a command names what that command opens.
    assert!(patterns("grep \"$(cat ~/.ssh/id_rsa)\" notes.md").is_empty());
    assert!(patterns("rg -e \"`cat .env`\" logs/").is_empty());
    // A keyword or wrapper only starts a program where it is one itself.
    assert!(patterns("cat do grep .env").is_empty());
    assert_eq!(patterns("sudo grep .env x.log"), [".env"]);
}

/// A search ends at a separator, also one after an option's value, and at a
/// new line of the script: a later command's words are not its patterns.
#[test]
fn a_search_ends_with_its_command() {
    assert_eq!(
        patterns("grep -e token; cat -e .env; echo done"),
        ["token;"]
    );
    assert_eq!(patterns("rg -g '*.md'; cat .env"), Vec::<String>::new());
    assert_eq!(
        patterns("rg -n DATABASE_URL src/\nnode -e \"require('fs').readFileSync('.env')\" || true"),
        ["DATABASE_URL"]
    );
    assert_eq!(
        patterns("rg token logs/\ncat -e .env\necho done"),
        ["token"]
    );
    assert_eq!(patterns("rg -e\ncat .env"), Vec::<String>::new());
    // A new line in quotes, or after `\\`, does not end it.
    assert_eq!(patterns("rg 'a\nb' logs/"), ["a\nb"]);
    assert_eq!(patterns("grep \\\n  -e .env logs/"), [".env"]);
    // A comment's quote does not hide the next line.
    assert_eq!(patterns("# don't\ngrep .env x.log"), [".env"]);
}

#[test]
fn hostname_program_paths_are_not_search_patterns() {
    for script in [
        "rg --hostname-bin .env token .npmrc",
        "rg --hostname-bin=.env token .npmrc",
        "rg --hostname-bin .env --hostname-bin .npmrc token .profile",
        "rg --hostname-bin .env -- token .npmrc",
        "rg --hostname-bin .env -e token .npmrc",
    ] {
        assert_eq!(patterns(script), ["token"], "{script}");
    }
    assert_eq!(patterns("rg --hostname-bin hostname .env logs"), [".env"]);
}

#[test]
fn exclude_from_abbreviations_keep_the_file_out_of_search_patterns() {
    for option in [
        "--exclude-f",
        "--exclude-fr",
        "--exclude-fro",
        "--exclude-from",
    ] {
        for script in [
            format!("grep {option} .env token .npmrc"),
            format!("grep {option}=.env token .npmrc"),
            format!("grep {option} .env -e token .npmrc"),
        ] {
            assert_eq!(patterns(&script), ["token"], "{script}");
        }
    }
    assert_eq!(patterns("grep --exclude-fr excludes .env logs"), [".env"]);
}

#[test]
fn path_option_recognition_respects_dialect_and_command_boundaries() {
    for script in [
        "grep --hostname-bin .env token logs",
        "rg --exclude-fr .env token logs",
        "grep --exclude- .env token logs",
        "grep --exclude-from-extra .env token logs",
    ] {
        assert_eq!(patterns(script), [".env"], "{script}");
    }
    for script in [
        "rg --hostname-bin .env; cat .npmrc",
        "grep --exclude-fr .env; cat .npmrc",
        "rg --hostname-bin\ncat .env",
        "grep --exclude-fr\ncat .env",
    ] {
        assert_eq!(patterns(script), Vec::<String>::new(), "{script}");
    }
}

/// Where the operands are unclear, nothing is marked.
#[test]
fn unclear_operands_are_not_patterns() {
    assert!(patterns("grep <(cat .env) logs").is_empty());
    assert_eq!(patterns("rg --pre cat token .env"), ["token"]);
}

#[test]
fn control_flow_conditions_recognize_search_patterns() {
    for script in [
        "if grep -q .env x.log; then :; fi",
        "if false; then :; elif grep -q .env x.log; then :; fi",
        "while grep -q .env x.log; do :; done",
        "until rg -q .env x.log; do :; done",
    ] {
        assert_eq!(patterns(script), [".env"], "{script}");
    }
}

#[test]
fn control_flow_words_in_arguments_do_not_start_searches() {
    for keyword in ["if", "elif", "while", "until"] {
        let script = format!("cat {keyword} grep .env");
        assert_eq!(patterns(&script), Vec::<String>::new(), "{script}");
    }
}

#[test]
fn control_flow_search_paths_remain_paths() {
    for script in [
        "if grep -q token .env; then :; fi",
        "if false; then :; elif grep -q token .env; then :; fi",
        "while grep -q token .env; do :; done",
        "until rg -q token .env; do :; done",
    ] {
        assert_eq!(patterns(script), ["token"], "{script}");
    }
}

/// #1480 item 1: a program word shaped like an assignment is one: the shell
/// runs the word after it (`.env` here), so `.env` is no grep pattern.
#[test]
fn assignment_shaped_program_words_do_not_start_searches() {
    for script in [
        "X=1/grep .env logs",
        "LC_ALL=C X=1/grep .env logs",
        "sudo X=1/rg .env logs",
        "env X=1/grep .env logs",
        "cat logs; X=1/grep .env logs",
        // After these the word is the program, which is not taken either,
        // also quoted or escaped (as on main).
        "nohup X=1/grep .env logs",
        "command X=1/grep .env logs",
        "nohup \"X=1/grep\" .env logs",
        "command 'X=1/grep' .env logs",
        "exec X\\=1/grep .env logs",
        "nohup \"X\"=1/rg .env logs",
        // A search it declines still ends where the command ends: no later
        // word of it starts another one.
        "X=1/grep rg .env logs",
        "X=1/grep sh -c rg .env logs",
        "'X=1' grep sh -c rg .env logs",
    ] {
        assert_eq!(patterns(script), Vec::<String>::new(), "{script}");
    }
    // Run directly, without a shell: `X=1/grep` is a program path then.
    let command = ["X=1/grep", "rg", ".env", "logs"].map(String::from);
    let words = super::super::command_words(&command, /*windows*/ false);
    let marked: Vec<&String> = search_pattern_indices(&command, &words, /*windows*/ false)
        .into_iter()
        .map(|index| &words[index])
        .collect();
    assert!(!marked.iter().any(|word| *word == ".env"), "{marked:?}");
    // A program path that only contains `=` past a slash is no assignment.
    assert_eq!(patterns("./X=1/grep .env logs"), [".env"]);
}

/// #1480 item 2: an assignment whose name or `=` is quoted or escaped is a
/// program name to the shell, so the search after it is an argument.
#[test]
fn quoted_or_escaped_assignment_words_are_not_prefixes() {
    for script in [
        "'X=1' grep .env logs",
        "\"X=1\" grep .env logs",
        "X\\=1 grep .env logs",
        "\"X\"=1 grep .env logs",
        "X'='1 grep .env logs",
        "LC_ALL=C 'X=1' rg .env logs",
    ] {
        assert_eq!(patterns(script), Vec::<String>::new(), "{script}");
    }
    // Only the value is quoted: still an assignment.
    for script in [
        "X='1' grep .env logs",
        "X=\"a b\" grep .env logs",
        "X=a\\ b rg .env logs",
    ] {
        assert_eq!(patterns(script), [".env"], "{script}");
    }
}

/// #1480 item 3: the `nohup`/`command`/`exec` exception reaches only the
/// word right after them.
#[test]
fn program_prefix_exception_reaches_only_the_next_word() {
    for script in [
        "nohup env X=1 grep .env logs",
        "X=1 nohup grep .env logs",
        "X=1 command grep .env logs",
        "exec env LC_ALL=C rg .env logs",
    ] {
        assert_eq!(patterns(script), [".env"], "{script}");
    }
}

/// #1480 item 5: a long chain of assignments costs one walk back, at the
/// search program, and is still recognised.
#[test]
fn long_assignment_chains_before_a_search_are_recognised() {
    let chain = (0..5_000).map(|n| format!("A{n}=1 ")).collect::<String>();
    assert_eq!(patterns(&format!("{chain}grep .env logs")), [".env"]);
    assert_eq!(
        patterns(&format!("{chain}cat grep .env logs")),
        Vec::<String>::new()
    );
}
