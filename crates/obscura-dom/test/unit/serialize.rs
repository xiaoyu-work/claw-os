use crate::tree_sink::parse_html;

#[test]
fn test_outer_html() {
    let tree = parse_html(r#"<div id="test"><p>Hello</p></div>"#);
    let div = tree.get_element_by_id("test").unwrap();
    let html = tree.outer_html(div);
    assert!(html.contains(r#"<div id="test">"#));
    assert!(html.contains("<p>Hello</p>"));
    assert!(html.contains("</div>"));
}

#[test]
fn test_inner_html() {
    let tree = parse_html(r#"<div id="test"><p>Hello</p><p>World</p></div>"#);
    let div = tree.get_element_by_id("test").unwrap();
    let html = tree.inner_html(div);
    assert!(html.contains("<p>Hello</p>"));
    assert!(html.contains("<p>World</p>"));
    assert!(!html.contains("<div"));
}

#[test]
fn test_serialize_attributes() {
    let tree = parse_html(r#"<a href="https://example.com" class="link">Click</a>"#);
    let a = tree.query_selector("a").unwrap().unwrap();
    let html = tree.outer_html(a);
    assert!(html.contains("href=\"https://example.com\""));
    assert!(html.contains("class=\"link\""));
}

#[test]
fn test_serialize_special_chars() {
    let tree = parse_html("<p>Hello &amp; World &lt;3</p>");
    let p = tree.query_selector("p").unwrap().unwrap();
    let html = tree.outer_html(p);
    assert!(html.contains("&amp;"));
    assert!(html.contains("&lt;"));
}

#[test]
fn test_void_elements() {
    let tree = parse_html(r#"<img src="test.png"><br>"#);
    let img = tree.query_selector("img").unwrap().unwrap();
    let html = tree.outer_html(img);
    assert!(html.contains("<img"));
    assert!(!html.contains("</img>"));
}
