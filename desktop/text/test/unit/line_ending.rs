#[test]
fn test_line_iter() {
    let string = "LF\nCRLF\r\nCR\rLFCR\n\rNONE";
    let mut iter = LineIter::new(string);
    assert_eq!(iter.next(), Some((0..2, LineEnding::Lf)));
    assert_eq!(iter.next(), Some((3..7, LineEnding::CrLf)));
    assert_eq!(iter.next(), Some((9..11, LineEnding::Cr)));
    assert_eq!(iter.next(), Some((12..16, LineEnding::LfCr)));
    assert_eq!(iter.next(), Some((18..22, LineEnding::None)));
}
