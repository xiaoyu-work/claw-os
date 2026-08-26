use super::*;

fn result(id: u32, name: &str, category: SearchResultCategory) -> SearchResult {
    SearchResult {
        id,
        name: name.into(),
        description: format!("{name} description"),
        icon: None,
        category_icon: None,
        window: None,
        category,
    }
}

#[test]
fn all_orders_suggestions_before_real_recents_and_deduplicates() {
    let duplicate = result(3, "Report", SearchResultCategory::Recent);
    let results = vec![
        result(1, "Report", SearchResultCategory::Files),
        result(2, "Terminal", SearchResultCategory::Commands),
        duplicate,
        result(4, "Notes", SearchResultCategory::Recent),
    ];

    assert_eq!(
        filtered_result_indices(&results, ResultFilter::All),
        vec![0, 1, 3]
    );
}

#[test]
fn category_filter_indices_stay_tied_to_source_results() {
    let results = vec![
        result(10, "Files", SearchResultCategory::Files),
        result(20, "Settings", SearchResultCategory::Settings),
        result(30, "More files", SearchResultCategory::Files),
    ];
    let indices = filtered_result_indices(&results, ResultFilter::Files);

    assert_eq!(indices, vec![0, 2]);
    assert_eq!(results[indices[1]].id, 30);
}

#[test]
fn ctrl_number_shortcuts_match_visible_positions() {
    assert_eq!(ctrl_number_index("1"), Some(0));
    assert_eq!(ctrl_number_index("9"), Some(8));
    assert_eq!(ctrl_number_index("0"), Some(9));
}
