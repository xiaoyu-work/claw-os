use super::*;

#[test]
fn test_parse_simple_html() {
    let tree = parse_html("<html><head></head><body><h1>Hello</h1></body></html>");
    assert!(tree.len() > 3);
    let text = tree.text_content(tree.document());
    assert!(text.contains("Hello"));
}

#[test]
fn test_parse_with_attributes() {
    let tree = parse_html(r#"<div id="main" class="container">Text</div>"#);
    let main = tree.get_element_by_id("main");
    assert!(main.is_some());
    let node = tree.get_node(main.unwrap()).unwrap();
    assert_eq!(node.get_attribute("class"), Some("container"));
}

#[test]
fn test_parse_nested_structure() {
    let tree = parse_html(
        r#"<html><body>
            <div id="outer">
                <p id="para">Hello <strong>World</strong></p>
                <ul>
                    <li>Item 1</li>
                    <li>Item 2</li>
                </ul>
            </div>
        </body></html>"#,
    );

    let outer = tree.get_element_by_id("outer").unwrap();
    let text = tree.text_content(outer);
    assert!(text.contains("Hello"));
    assert!(text.contains("World"));
    assert!(text.contains("Item 1"));
    assert!(text.contains("Item 2"));
}

#[test]
fn test_parse_malformed_html() {
    let tree = parse_html("<div><p>Unclosed paragraph<p>Another<div>Nested wrong</div>");
    assert!(tree.len() > 3);
    let text = tree.text_content(tree.document());
    assert!(text.contains("Unclosed paragraph"));
    assert!(text.contains("Another"));
}

#[test]
fn test_parse_doctype() {
    let tree = parse_html("<!DOCTYPE html><html><body>Hello</body></html>");
    let first_child = tree.children(tree.document())[0];
    let node = tree.get_node(first_child).unwrap();
    assert!(matches!(node.data, NodeData::Doctype { .. }));
}

#[test]
fn test_parse_fragment() {
    let tree = parse_fragment("<p>Hello</p><p>World</p>");
    let text = tree.text_content(tree.document());
    assert!(text.contains("Hello"));
    assert!(text.contains("World"));
}
