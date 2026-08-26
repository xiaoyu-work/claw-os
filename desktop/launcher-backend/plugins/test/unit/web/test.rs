use crate::web::parse_favicon;

async fn fetch(url: &str) -> String {
    reqwest::get(url).await.unwrap().text().await.unwrap()
}

#[tokio::test]
async fn should_parse_favicon_url_github() {
    let html = fetch("https://github.com").await;

    let icon_url = parse_favicon(&html);
    assert_eq!(
        Some("https://github.githubassets.com/favicons/favicon.png".to_string()),
        icon_url
    );
}

#[tokio::test]
async fn should_parse_favicon_url_ddg() {
    // Ddg returns a relative path to its favicon
    let html = fetch("https://duckduckgo.com").await;

    let icon_url = parse_favicon(&html);
    assert_eq!(Some("/favicon.ico".to_string()), icon_url);
}

#[tokio::test]
async fn parse_favicon_url_google_returns_none() {
    // Google seems to set its favicon via javascript
    // hence there is no way to get the favicon from the page
    // source
    let html = fetch("https://google.com").await;

    let icon_url = parse_favicon(&html);
    assert!(icon_url.is_none());
}

#[tokio::test]
async fn should_parse_favicon_url_flathub() {
    // Ensure we don't match the commented icon in flathub page
    // <!-- <link rel="icon" type="image/x-icon" href="favicon.ico"> -->
    // <link rel="icon" type="image/png" href="/assets/themes/flathub/favicon-32x32.png">
    let html = fetch("https://flathub.org").await;

    let icon_url = parse_favicon(&html);
    assert_eq!(
        Some("/assets/themes/flathub/favicon-32x32.png".to_string()),
        icon_url
    );
}

#[tokio::test]
async fn should_parse_favicon_url_aliexpress() {
    // Aliexpress icon href start with two slash :`href="//ae01.alicdn.com/images/eng/wholesale/icon/aliexpress.ico"`

    let client = reqwest::Client::new();

    let html = client
        .get("https://aliexpress.com")
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    let icon_url = parse_favicon(&html);
    assert_eq!(
        Some("https://ae01.alicdn.com/images/eng/wholesale/icon/aliexpress.ico".to_string()),
        icon_url
    );
}
