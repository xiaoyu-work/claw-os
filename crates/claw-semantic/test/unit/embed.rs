use super::*;

#[test]
fn stub_is_deterministic_and_unit_norm() {
    let e = StubEmbedder;
    let a = e.embed(&["hello".into()]).unwrap();
    let b = e.embed(&["hello".into()]).unwrap();
    assert_eq!(a, b);
    assert_eq!(a[0].len(), EMBED_DIM);
    let n: f32 = a[0].iter().map(|x| x * x).sum::<f32>().sqrt();
    assert!((n - 1.0).abs() < 1e-3, "norm = {n}");
}
