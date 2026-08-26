use crate::tree_sink::parse_html;

#[test]
fn test_query_selector_tag() {
    let tree = parse_html("<html><body><h1>Title</h1><p>Text</p></body></html>");
    let result = tree.query_selector("h1").unwrap();
    assert!(result.is_some());
    let node = tree.get_node(result.unwrap()).unwrap();
    assert_eq!(node.as_element().unwrap().local.as_ref(), "h1");
}

#[test]
fn test_query_selector_class() {
    let tree =
        parse_html(r#"<div class="foo bar">Content</div><div class="baz">Other</div>"#);
    let result = tree.query_selector(".foo").unwrap();
    assert!(result.is_some());
    let node = tree.get_node(result.unwrap()).unwrap();
    assert_eq!(node.get_attribute("class"), Some("foo bar"));
}

#[test]
fn test_query_selector_id() {
    let tree = parse_html(r#"<div id="main">Content</div>"#);
    let result = tree.query_selector("#main").unwrap();
    assert!(result.is_some());
}

#[test]
fn test_query_selector_all() {
    let tree = parse_html("<ul><li>1</li><li>2</li><li>3</li></ul>");
    let results = tree.query_selector_all("li").unwrap();
    assert_eq!(results.len(), 3);
}

#[test]
fn test_query_selector_descendant() {
    let tree =
        parse_html(r#"<div id="outer"><div id="inner"><span>Target</span></div></div>"#);
    let result = tree.query_selector("#outer span").unwrap();
    assert!(result.is_some());
    let node = tree.get_node(result.unwrap()).unwrap();
    assert_eq!(node.as_element().unwrap().local.as_ref(), "span");
}

#[test]
fn test_query_selector_attribute() {
    let tree = parse_html(
        r#"<input type="text" name="user"><input type="password" name="pass">"#,
    );
    let result = tree.query_selector(r#"input[type="password"]"#).unwrap();
    assert!(result.is_some());
    let node = tree.get_node(result.unwrap()).unwrap();
    assert_eq!(node.get_attribute("name"), Some("pass"));
}

#[test]
fn test_query_selector_no_match() {
    let tree = parse_html("<div>Hello</div>");
    let result = tree.query_selector("span").unwrap();
    assert!(result.is_none());
}

#[test]
fn test_query_selector_complex() {
    let tree = parse_html(
        r#"<div class="container">
            <ul class="list">
                <li class="item active">First</li>
                <li class="item">Second</li>
                <li class="item active">Third</li>
            </ul>
        </div>"#,
    );
    let results = tree.query_selector_all(".list .item.active").unwrap();
    assert_eq!(results.len(), 2);
}
