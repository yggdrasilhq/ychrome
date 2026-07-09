//! Password generation.
//!
//! Local and offline — there is no reason to ask a server to roll dice, and
//! `rbw generate` shelling out was one more process in the fill path.
//!
//! Characters are drawn with `gen_range`, which rejects out-of-range samples
//! rather than taking a modulus, so every character of the alphabet is equally
//! likely. (A `% len` would quietly bias toward the first few characters.)

use rand::Rng;
use zeroize::Zeroizing;

const LOWER: &str = "abcdefghijkmnopqrstuvwxyz";
const UPPER: &str = "ABCDEFGHJKLMNPQRSTUVWXYZ";
const DIGITS: &str = "23456789";
const SYMBOLS: &str = "!#$%&*+-=?@^_";

/// The shortest password worth generating. Below this, guaranteeing one
/// character from each class stops leaving room for randomness.
pub const MIN_LENGTH: usize = 8;
pub const DEFAULT_LENGTH: usize = 20;

/// Generate a password of `length` characters, guaranteeing at least one
/// character from each enabled class (so a site that demands "one digit, one
/// symbol" never rejects it). Look-alikes — `l`, `I`, `O`, `0`, `1` — are
/// excluded from every class, because these get read aloud and typed by hand.
///
/// Zeroized on drop; the caller decides where it goes.
pub fn generate_password(length: usize, symbols: bool) -> Zeroizing<String> {
    let length = length.max(MIN_LENGTH);
    let mut classes: Vec<&str> = vec![LOWER, UPPER, DIGITS];
    if symbols {
        classes.push(SYMBOLS);
    }
    let alphabet: Vec<char> = classes.iter().flat_map(|class| class.chars()).collect();
    let mut rng = rand::thread_rng();

    // One guaranteed character per class, then fill, then shuffle so the
    // guaranteed ones are not pinned to the front.
    let mut chars: Vec<char> = classes
        .iter()
        .map(|class| pick(&mut rng, class.chars().collect::<Vec<_>>().as_slice()))
        .collect();
    while chars.len() < length {
        chars.push(pick(&mut rng, &alphabet));
    }
    for i in (1..chars.len()).rev() {
        chars.swap(i, rng.gen_range(0..=i));
    }
    Zeroizing::new(chars.into_iter().collect())
}

fn pick(rng: &mut impl Rng, from: &[char]) -> char {
    from[rng.gen_range(0..from.len())]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_passwords_have_the_right_shape() {
        for symbols in [true, false] {
            let password = generate_password(24, symbols);
            assert_eq!(password.chars().count(), 24);
            assert!(password.chars().any(|c| c.is_ascii_lowercase()));
            assert!(password.chars().any(|c| c.is_ascii_uppercase()));
            assert!(password.chars().any(|c| c.is_ascii_digit()));
            assert_eq!(
                password.chars().any(|c| SYMBOLS.contains(c)),
                symbols,
                "symbols requested = {symbols}"
            );
            // Look-alikes never appear, whatever the class. (The password
            // itself is never printed, even on failure.)
            assert!(
                !password.chars().any(|c| "lIO01".contains(c)),
                "generated password contains a look-alike character"
            );
        }
    }

    #[test]
    fn length_floor_is_enforced_and_output_is_random() {
        assert_eq!(generate_password(1, true).chars().count(), MIN_LENGTH);
        assert_ne!(*generate_password(20, true), *generate_password(20, true));
    }

    // The guaranteed characters must not always land at the front — that would
    // make the first three positions predictable by class.
    #[test]
    fn guaranteed_characters_are_shuffled_into_the_password() {
        let first_is_lower = (0..64)
            .filter(|_| {
                generate_password(20, true)
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_lowercase())
            })
            .count();
        assert!(
            (1..64).contains(&first_is_lower),
            "first char was lowercase {first_is_lower}/64 times — not shuffled"
        );
    }
}
