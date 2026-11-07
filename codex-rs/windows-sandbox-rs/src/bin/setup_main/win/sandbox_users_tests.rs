use super::PASSWORD_CHARS;
use super::PASSWORD_LEN;
use super::has_every_character_class;
use super::random_password;
use super::random_password_with;
use pretty_assertions::assert_eq;

fn index_of(c: u8) -> usize {
    PASSWORD_CHARS
        .iter()
        .position(|&b| b == c)
        .expect("char in PASSWORD_CHARS")
}

#[test]
fn password_missing_a_character_class_is_redrawn() {
    // #1776: the first draw is all lowercase, which Windows' complexity policy
    // rejects (NERR 2245); the generator must not hand it to NetUserSetInfo.
    let rejected = vec![index_of(b'q'); PASSWORD_LEN];
    let accepted: Vec<usize> = b"Aa1!"
        .iter()
        .cycle()
        .take(PASSWORD_LEN)
        .map(|&c| index_of(c))
        .collect();
    let mut script = rejected.into_iter().chain(accepted);
    let mut picks = 0;

    let password = random_password_with(|n| {
        assert_eq!(n, PASSWORD_CHARS.len());
        picks += 1;
        script
            .next()
            .expect("generator drew more than two candidates")
    });

    assert_eq!(password, "Aa1!".repeat(PASSWORD_LEN / 4));
    assert_eq!(picks, 2 * PASSWORD_LEN);
}

#[test]
fn character_classes_are_all_required() {
    assert!(has_every_character_class("Aa1!"));
    assert!(!has_every_character_class("abcdefghijklmnopqrstuvwx"));
    assert!(!has_every_character_class("ABCDEFabcdef012345678901"));
    assert!(!has_every_character_class("ABCDEFabcdef!@#$%^&*()-_"));
    assert!(!has_every_character_class("abcdef012345!@#$%^&*()-_"));
    assert!(!has_every_character_class("ABCDEF012345!@#$%^&*()-_"));
}

#[test]
fn generated_passwords_meet_windows_complexity() {
    for _ in 0..20_000 {
        let password = random_password();
        assert_eq!(password.len(), PASSWORD_LEN);
        assert!(password.bytes().all(|b| PASSWORD_CHARS.contains(&b)));
        assert!(has_every_character_class(&password));
    }
}
