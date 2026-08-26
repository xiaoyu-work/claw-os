use super::Binding;
use crate::shortcuts::Modifiers;
use std::str::FromStr;

#[test]
fn binding_from_str() {
    assert_eq!(
        Binding::from_str("Super+Q"),
        Ok(Binding::new(
            Modifiers::new().logo(),
            Some(xkbcommon::xkb::Keysym::from_char('q'))
        ))
    );

    assert_eq!(
        Binding::from_str("Super+Ctrl+Alt+F"),
        Ok(Binding::new(
            Modifiers::new().logo().ctrl().alt(),
            Some(xkbcommon::xkb::Keysym::from_char('f'))
        ))
    );

    assert_eq!(
        Binding::from_str("Super+Down"),
        Ok(Binding::new(
            Modifiers::new().logo(),
            Some(xkbcommon::xkb::Keysym::Down)
        ))
    );

    assert_eq!(
        Binding::from_str("XF86MonBrightnessDown"),
        Ok(Binding::new(
            Modifiers::new(),
            Some(xkbcommon::xkb::Keysym::XF86_MonBrightnessDown)
        ))
    );

    assert_eq!(
        Binding::from_str("Super+space"),
        Ok(Binding::new(
            Modifiers::new().logo(),
            Some(xkbcommon::xkb::Keysym::space)
        ))
    );

    // Case-insensitive
    assert_eq!(
        Binding::from_str("super+up"),
        Ok(Binding::new(
            Modifiers::new().logo(),
            Some(xkbcommon::xkb::Keysym::Up)
        ))
    );

    // Must have a non-modifier key.
    assert!(matches!(Binding::from_str("Super+Shift"), Err(_)));

    // Can't have multiple non-modifier keys.
    assert!(matches!(Binding::from_str("Super+Up+Down"), Err(_)));

    // At least one key is required.
    assert!(matches!(Binding::from_str(" "), Err(_)));
}

#[test]
fn binding_from_str_partial() {
    // Non-modifier key not required.
    assert_eq!(
        Binding::from_str_partial("Super+Ctrl+Alt"),
        Ok(Binding::new(Modifiers::new().logo().ctrl().alt(), None,))
    );

    // Can't have multiple non-modifier keys.
    assert!(matches!(Binding::from_str("Super+Up+Down"), Err(_)));

    // At least one key is required.
    assert!(matches!(Binding::from_str(" "), Err(_)));
}
