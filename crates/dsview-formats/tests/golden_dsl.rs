//! The Rust reader must deliver exactly what the C session driver delivers:
//! compares a summary of every corpus capture with testdata/golden/dsl/*.json
//! (written by dev/dsview-oracle's `oracle-dsl`).

use dsview_testkit::corpus::Corpus;
use dsview_testkit::dsl_summary::summary;
use dsview_testkit::repo_root;
use serde_json::Value;

#[test]
fn rust_reader_matches_c_session_driver() {
    let root = repo_root();
    let corpus = Corpus::load(&root).unwrap();
    corpus.verify(&root).unwrap();
    for entry in &corpus.files {
        let golden_path = root.join(format!(
            "testdata/golden/dsl/{}.json",
            Corpus::golden_name(entry)
        ));
        let golden: Value =
            serde_json::from_str(&std::fs::read_to_string(&golden_path).unwrap()).unwrap();
        let actual = summary(&root.join(&entry.path));
        assert_eq!(actual, golden, "{}", entry.path);
    }
}
