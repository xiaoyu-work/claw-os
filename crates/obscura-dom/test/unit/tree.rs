use super::*;

#[test]
fn test_new_tree_has_document() {
    let tree = DomTree::new();
    assert_eq!(tree.len(), 1);
    let node = tree.get_node(tree.document()).unwrap();
    assert!(node.is_document());
}

#[test]
fn test_append_child() {
    let tree = DomTree::new();
    let child = tree.new_node(NodeData::Text {
        contents: "hello".into(),
    });
    let doc = tree.document();
    tree.append_child(doc, child);

    assert_eq!(tree.len(), 2);
    let doc_node = tree.get_node(doc).unwrap();
    assert_eq!(doc_node.first_child, Some(child));
    assert_eq!(doc_node.last_child, Some(child));

    let child_node = tree.get_node(child).unwrap();
    assert_eq!(child_node.parent, Some(doc));
}

#[test]
fn test_multiple_children() {
    let tree = DomTree::new();
    let doc = tree.document();
    let c1 = tree.new_node(NodeData::Text { contents: "a".into() });
    let c2 = tree.new_node(NodeData::Text { contents: "b".into() });
    let c3 = tree.new_node(NodeData::Text { contents: "c".into() });
    tree.append_child(doc, c1);
    tree.append_child(doc, c2);
    tree.append_child(doc, c3);

    assert_eq!(tree.children(doc), vec![c1, c2, c3]);
}

#[test]
fn test_detach() {
    let tree = DomTree::new();
    let doc = tree.document();
    let c1 = tree.new_node(NodeData::Text { contents: "a".into() });
    let c2 = tree.new_node(NodeData::Text { contents: "b".into() });
    tree.append_child(doc, c1);
    tree.append_child(doc, c2);

    tree.detach(c1);
    assert_eq!(tree.children(doc), vec![c2]);
}

#[test]
fn test_insert_before() {
    let tree = DomTree::new();
    let doc = tree.document();
    let c1 = tree.new_node(NodeData::Text { contents: "a".into() });
    let c2 = tree.new_node(NodeData::Text { contents: "b".into() });
    let c3 = tree.new_node(NodeData::Text { contents: "c".into() });
    tree.append_child(doc, c1);
    tree.append_child(doc, c3);
    tree.insert_before(c3, c2);

    assert_eq!(tree.children(doc), vec![c1, c2, c3]);
}

#[test]
fn test_text_content() {
    let tree = DomTree::new();
    let doc = tree.document();
    let div = tree.new_node(NodeData::Element {
        name: QualName::new(None, ns!(html), local_name!("div")),
        attrs: vec![],
        template_contents: None,
        mathml_annotation_xml_integration_point: false,
    });
    tree.append_child(doc, div);

    let t1 = tree.new_node(NodeData::Text { contents: "Hello ".into() });
    let t2 = tree.new_node(NodeData::Text { contents: "World".into() });
    tree.append_child(div, t1);
    tree.append_child(div, t2);

    assert_eq!(tree.text_content(div), "Hello World");
}

#[test]
fn test_get_element_by_id() {
    let tree = DomTree::new();
    let doc = tree.document();
    let div = tree.new_node(NodeData::Element {
        name: QualName::new(None, ns!(html), local_name!("div")),
        attrs: vec![Attribute {
            name: QualName::new(None, Namespace::default(), LocalName::from("id")),
            value: "main".into(),
        }],
        template_contents: None,
        mathml_annotation_xml_integration_point: false,
    });
    tree.append_child(doc, div);

    assert_eq!(tree.get_element_by_id("main"), Some(div));
    assert_eq!(tree.get_element_by_id("nonexistent"), None);
}

#[test]
fn test_append_text_merges() {
    let tree = DomTree::new();
    let doc = tree.document();
    tree.append_text(doc, "Hello ");
    tree.append_text(doc, "World");

    assert_eq!(tree.children(doc).len(), 1);
    assert_eq!(tree.text_content(doc), "Hello World");
}

#[test]
fn test_remove_subtree() {
    let tree = DomTree::new();
    let doc = tree.document();
    let div = tree.new_node(NodeData::Element {
        name: QualName::new(None, ns!(html), local_name!("div")),
        attrs: vec![],
        template_contents: None,
        mathml_annotation_xml_integration_point: false,
    });
    tree.append_child(doc, div);
    let text = tree.new_node(NodeData::Text { contents: "hi".into() });
    tree.append_child(div, text);

    assert_eq!(tree.len(), 3);
    tree.remove(div);
    assert_eq!(tree.len(), 1);
}
