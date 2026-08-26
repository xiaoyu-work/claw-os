use crate::tree::{DomTree, NodeData, NodeId};

impl DomTree {
    pub fn outer_html(&self, node_id: NodeId) -> String {
        let mut buf = String::new();
        self.serialize_node(node_id, true, &mut buf);
        buf
    }

    pub fn inner_html(&self, node_id: NodeId) -> String {
        let mut buf = String::new();
        self.serialize_children(node_id, &mut buf);
        buf
    }

    fn serialize_node(&self, node_id: NodeId, include_self: bool, buf: &mut String) {
        let node = match self.get_node(node_id) {
            Some(n) => n,
            None => return,
        };

        match &node.data {
            NodeData::Document => {
                self.serialize_children(node_id, buf);
            }
            NodeData::Doctype { name, .. } => {
                buf.push_str("<!DOCTYPE ");
                buf.push_str(name);
                buf.push('>');
            }
            NodeData::Element { name, attrs, .. } => {
                let tag = name.local.as_ref();
                if include_self {
                    buf.push('<');
                    buf.push_str(tag);
                    for attr in attrs {
                        buf.push(' ');
                        let attr_name = attr.name.local.as_ref();
                        buf.push_str(attr_name);
                        buf.push_str("=\"");
                        escape_attr(&attr.value, buf);
                        buf.push('"');
                    }
                    buf.push('>');
                }

                if !is_void_element(tag) {
                    self.serialize_children(node_id, buf);
                    if include_self {
                        buf.push_str("</");
                        buf.push_str(tag);
                        buf.push('>');
                    }
                }
            }
            NodeData::Text { contents } => {
                let parent_is_raw = node.parent
                    .and_then(|pid| {
                        self.with_node(pid, |p| {
                            p.as_element()
                                .map(|name| is_raw_text_element(name.local.as_ref()))
                                .unwrap_or(false)
                        })
                    })
                    .unwrap_or(false);

                if parent_is_raw {
                    buf.push_str(contents);
                } else {
                    escape_text(contents, buf);
                }
            }
            NodeData::Comment { contents } => {
                buf.push_str("<!--");
                buf.push_str(contents);
                buf.push_str("-->");
            }
            NodeData::ProcessingInstruction { target, data } => {
                buf.push_str("<?");
                buf.push_str(target);
                buf.push(' ');
                buf.push_str(data);
                buf.push('>');
            }
        }
    }

    fn serialize_children(&self, node_id: NodeId, buf: &mut String) {
        for child_id in self.children(node_id) {
            self.serialize_node(child_id, true, buf);
        }
    }
}

fn escape_text(s: &str, buf: &mut String) {
    for c in s.chars() {
        match c {
            '&' => buf.push_str("&amp;"),
            '<' => buf.push_str("&lt;"),
            '>' => buf.push_str("&gt;"),
            _ => buf.push(c),
        }
    }
}

fn escape_attr(s: &str, buf: &mut String) {
    for c in s.chars() {
        match c {
            '&' => buf.push_str("&amp;"),
            '"' => buf.push_str("&quot;"),
            _ => buf.push(c),
        }
    }
}

fn is_void_element(tag: &str) -> bool {
    matches!(
        tag,
        "area" | "base" | "br" | "col" | "embed" | "hr" | "img" | "input" | "link" | "meta"
            | "param" | "source" | "track" | "wbr"
    )
}

fn is_raw_text_element(tag: &str) -> bool {
    matches!(tag, "script" | "style" | "textarea" | "title")
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/serialize.rs"
    ));
}
