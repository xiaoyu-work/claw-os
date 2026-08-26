use super::*;

fn sample_model() -> (Model<SingleSelect>, Vec<Entity>) {
    let mut ids = Vec::new();
    let model = Model::builder()
        .insert(|b| b.text("Tab1").with_id(|id| ids.push(id)))
        .insert(|b| b.text("Tab2").with_id(|id| ids.push(id)))
        .insert(|b| b.text("Tab3").with_id(|id| ids.push(id)))
        .insert(|b| b.text("Tab4").with_id(|id| ids.push(id)))
        .build();
    (model, ids)
}

fn order_of(model: &Model<SingleSelect>) -> Vec<Entity> {
    model.iter().collect()
}

#[test]
fn reorder_inserts_before_target() {
    let (mut model, ids) = sample_model();
    assert!(model.reorder(ids[3], ids[1], InsertPosition::Before));
    assert_eq!(order_of(&model), vec![ids[0], ids[3], ids[1], ids[2]]);
}

#[test]
fn reorder_inserts_after_target() {
    let (mut model, ids) = sample_model();
    assert!(model.reorder(ids[0], ids[2], InsertPosition::After));
    assert_eq!(order_of(&model), vec![ids[1], ids[2], ids[0], ids[3]]);
}

#[test]
fn reorder_rejects_invalid_entities() {
    let (mut model, ids) = sample_model();
    assert!(!model.reorder(ids[0], ids[0], InsertPosition::After));
}
