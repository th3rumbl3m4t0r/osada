//! What a sign-in must show (§10.2, §10.7): the factors, and the rule the
//! account's OU sets. Nothing is required beyond a first factor unless an
//! administrator turns it on for an OU; a tier-0 OU requires FIDO2.

use crate::placement::OuId;

/// A factor shown in a sign-in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Factor {
    Password,
    Totp,
    /// A FIDO2 credential (security key or passkey); `verified` when the
    /// authenticator verified the user (PIN, fingerprint).
    Fido2 { verified: bool },
}

/// The rule for an account, from its OU and those above it. Each is the OU
/// where it was turned on; they only tighten down the tree.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SignInRule {
    /// Tier 0: FIDO2 required, TOTP does not count.
    pub tier0: Option<OuId>,
    /// A second factor (TOTP or FIDO2) required.
    pub second_factor: Option<OuId>,
}

/// What a sign-in still lacks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Missing {
    /// A password, or a FIDO2 credential that verified the user.
    FirstFactor,
    /// A second factor: TOTP or FIDO2.
    SecondFactor,
    /// Tier 0: FIDO2.
    Fido2,
}

impl SignInRule {
    /// Whether the factors shown are enough; if not, what is missing.
    pub fn check(&self, shown: &[Factor]) -> Result<(), Missing> {
        let password = shown.contains(&Factor::Password);
        let totp = shown.contains(&Factor::Totp);
        let fido2 = shown.iter().any(|f| matches!(f, Factor::Fido2 { .. }));
        // A FIDO2 credential that verified the user is two factors on its
        // own: something held and something known or been (§10.2).
        let passkey = shown.contains(&Factor::Fido2 { verified: true });
        if !(password || passkey) {
            return Err(Missing::FirstFactor);
        }
        if self.tier0.is_some() {
            return if passkey || (password && fido2) { Ok(()) } else { Err(Missing::Fido2) };
        }
        if self.second_factor.is_some() && !(passkey || (password && (totp || fido2))) {
            return Err(Missing::SecondFactor);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Factor::*;

    const KEY: Factor = Fido2 { verified: false };
    const PASSKEY: Factor = Fido2 { verified: true };

    #[test]
    fn nothing_more_unless_turned_on() {
        let r = SignInRule::default();
        assert_eq!(r.check(&[Password]), Ok(()));
        assert_eq!(r.check(&[PASSKEY]), Ok(()));
        assert_eq!(r.check(&[]), Err(Missing::FirstFactor));
        assert_eq!(r.check(&[Totp]), Err(Missing::FirstFactor));
        assert_eq!(r.check(&[KEY]), Err(Missing::FirstFactor));
    }

    #[test]
    fn second_factor_when_an_ou_asks() {
        let r = SignInRule { second_factor: Some(OuId(2)), ..Default::default() };
        assert_eq!(r.check(&[Password]), Err(Missing::SecondFactor));
        assert_eq!(r.check(&[Password, Totp]), Ok(()));
        assert_eq!(r.check(&[Password, KEY]), Ok(()));
        assert_eq!(r.check(&[PASSKEY]), Ok(()));
    }

    #[test]
    fn tier0_takes_fido2_only() {
        let r = SignInRule { tier0: Some(OuId(4)), second_factor: None };
        assert_eq!(r.check(&[Password]), Err(Missing::Fido2));
        assert_eq!(r.check(&[Password, Totp]), Err(Missing::Fido2));
        assert_eq!(r.check(&[Password, KEY]), Ok(()));
        assert_eq!(r.check(&[PASSKEY]), Ok(()));
        assert_eq!(r.check(&[KEY]), Err(Missing::FirstFactor));
    }
}
