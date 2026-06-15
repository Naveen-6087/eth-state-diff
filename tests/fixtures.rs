mod common;

use common::fixtures_dir;
use eth_state_diff::{Format, Report, StateDiff, Unit, parse_traces_str, render};

/// Traces captured from geth 1.17 (see tests/geth.rs) must still decode and
/// render exactly as recorded.
#[test]
fn geth_fixtures_render_golden_text() {
    let mut checked = 0;
    for entry in std::fs::read_dir(fixtures_dir()).expect("testdata/geth exists") {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let name = path.file_stem().unwrap().to_string_lossy().into_owned();
        let traces = parse_traces_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(traces.len(), 1, "{name}");
        let diff = StateDiff::from_prestate(&traces[0].diff);
        let report = Report::Net { label: name.clone(), transactions: 1, diff };
        let mut text = Vec::new();
        render(&report, Format::Text, Unit::Wei, &mut text).unwrap();
        let golden = std::fs::read_to_string(path.with_extension("txt")).unwrap();
        assert_eq!(String::from_utf8(text).unwrap(), golden, "{name}");
        checked += 1;
    }
    assert_eq!(checked, 4);
}
