use std::sync::OnceLock;

use i18n_embed::{
    DefaultLocalizer, LanguageLoader, Localizer,
    fluent::{FluentLanguageLoader, fluent_language_loader},
};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "i18n/"]
struct Localizations;

pub static LANGUAGE_LOADER: OnceLock<FluentLanguageLoader> = OnceLock::new();

#[macro_export]
macro_rules! fl {
    ($message_id:literal) => {{
        i18n_embed_fl::fl!($crate::localize::LANGUAGE_LOADER.get().unwrap(), $message_id)
    }};
    ($message_id:literal, $($args:expr),*) => {{
        i18n_embed_fl::fl!($crate::localize::LANGUAGE_LOADER.get().unwrap(), $message_id, $($args),*)
    }};
}

pub fn localize() {
    let loader = LANGUAGE_LOADER.get_or_init(|| {
        let loader: FluentLanguageLoader = fluent_language_loader!();
        loader
            .load_fallback_language(&Localizations)
            .expect("load fallback translations");
        loader
    });
    let localizer = DefaultLocalizer::new(loader, &Localizations);
    let requested = i18n_embed::DesktopLanguageRequester::requested_languages();
    if let Err(error) = localizer.select(&requested) {
        tracing::warn!("failed to select Agent UI language: {error}");
    }
}
