#![cfg(feature = "ci-live")]

use serde_json::Value;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

struct Run {
    succeeded: bool,
    stdout: String,
    stderr: String,
}

fn fsq(arguments: &[&str], stdin: &str) -> Run {
    let mut child = Command::new(env!("CARGO_BIN_EXE_fsq"))
        .args([
            "-p",
            &std::env::var("GCP_PROJECT").expect("GCP_PROJECT"),
            "-d",
            &std::env::var("GCP_ENTERPRISE_DATABASE_ID").expect("GCP_ENTERPRISE_DATABASE_ID"),
        ])
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("fsq starts");
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(stdin.as_bytes())
        .expect("stdin is written");
    let output = child.wait_with_output().expect("fsq finishes");
    Run {
        succeeded: output.status.success(),
        stdout: String::from_utf8(output.stdout).expect("UTF-8 output"),
        stderr: String::from_utf8(output.stderr).expect("UTF-8 output"),
    }
}

fn unique_name(prefix: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("a time after the epoch")
        .as_nanos();
    format!("{prefix}-{nanos}")
}

/// Deletes the documents of the collections it names when the test ends, even when it fails.
struct Collections(Vec<String>);

impl Drop for Collections {
    fn drop(&mut self) {
        for collection in &self.0 {
            fsq(
                &[
                    "sql",
                    "-e",
                    &format!("DELETE FROM `{collection}` WHERE TRUE"),
                ],
                "",
            );
        }
    }
}

fn exported_names(collection: &str, recursive: bool) -> Vec<String> {
    let mut arguments = vec!["export", collection];
    if recursive {
        arguments.push("--recursive");
    }
    let run = fsq(&arguments, "");
    assert!(run.succeeded, "{}", run.stderr);
    let mut names: Vec<String> = run
        .stdout
        .lines()
        .map(|line| {
            let document: Value = serde_json::from_str(line).expect("a JSON line");
            document["__name__"].as_str().expect("a name").to_string()
        })
        .collect();
    names.sort();
    names
}

#[test]
fn recursive_export_reaches_the_subcollections_of_a_missing_document() {
    let root = unique_name("fsq-live-tree");
    let _cleanup = Collections(vec![format!("{root}/x/b"), root.clone()]);
    let lines = format!(
        "{{\"__name__\":\"{root}/w\",\"title\":\"w\"}}\n{{\"__name__\":\"{root}/x/b/y\",\"title\":\"y\"}}\n"
    );
    assert!(fsq(&["import", "-"], &lines).succeeded);

    assert_eq!(exported_names(&root, false), [format!("{root}/w")]);
    assert_eq!(
        exported_names(&root, true),
        [format!("{root}/w"), format!("{root}/x/b/y")]
    );
}

fn lines_naming_d1_three_times(root: &str) -> String {
    format!(
        "{{\"__name__\":\"{root}/d1\",\"n\":1}}\n{{\"__name__\":\"{root}/d1\",\"n\":2}}\n\
         {{\"__name__\":\"{root}/d1\",\"n\":3}}\n{{\"__name__\":\"{root}/d2\",\"n\":4}}\n"
    )
}

#[test]
fn import_counts_a_repeated_document_by_its_conflict_policy() {
    let roots = [
        unique_name("fsq-live-stops"),
        unique_name("fsq-live-goes-on"),
        unique_name("fsq-live-replaces"),
    ];
    let _cleanup = Collections(roots.to_vec());

    let stops = fsq(
        &["import", "-", "--on-conflict", "fail"],
        &lines_naming_d1_three_times(&roots[0]),
    );
    assert!(!stops.succeeded);
    assert!(
        stops
            .stderr
            .contains("1 document written, 0 skipped, 1 failed"),
        "{}",
        stops.stderr
    );

    let goes_on = fsq(
        &["import", "-", "--on-conflict", "fail", "--keep-going"],
        &lines_naming_d1_three_times(&roots[1]),
    );
    assert!(!goes_on.succeeded);
    assert!(
        goes_on
            .stderr
            .contains("2 documents written, 0 skipped, 2 failed"),
        "{}",
        goes_on.stderr
    );
    assert_eq!(
        exported_names(&roots[1], false),
        [format!("{}/d1", roots[1]), format!("{}/d2", roots[1])]
    );

    let replaces = fsq(&["import", "-"], &lines_naming_d1_three_times(&roots[2]));
    assert!(replaces.succeeded, "{}", replaces.stderr);
    assert!(
        replaces
            .stderr
            .contains("4 documents written, 0 skipped, 0 failed"),
        "{}",
        replaces.stderr
    );
}

#[test]
fn sync_declares_the_indexes_of_every_group_of_the_file() {
    let groups = [unique_name("fsq-live-sync"), unique_name("fsq-live-sync")];
    let file: PathBuf = std::env::temp_dir().join(format!("{}.json", groups[0]));
    let indexes: Vec<Value> = groups
        .iter()
        .map(|group| {
            serde_json::json!({
                "collectionGroup": group,
                "queryScope": "COLLECTION",
                "fields": [
                    { "fieldPath": "a", "order": "ASCENDING" },
                    { "fieldPath": "b", "order": "DESCENDING" }
                ]
            })
        })
        .collect();
    std::fs::write(
        &file,
        serde_json::json!({ "indexes": indexes, "fieldOverrides": [] }).to_string(),
    )
    .expect("the file is written");
    let path = file.to_str().expect("a UTF-8 path");

    let run = fsq(
        &["--format", "jsonl", "--no-wait", "index", "sync", path],
        "",
    );

    for group in &groups {
        fsq(
            &["index", "delete", group, "a:asc", "b:desc", "--if-exists"],
            "",
        );
    }
    std::fs::remove_file(&file).expect("the file is removed");
    assert!(run.succeeded, "{}", run.stderr);
    let created: Vec<String> = run
        .stdout
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("a JSON line"))
        .filter(|row| row["action"] == "create")
        .map(|row| {
            row["collection_group"]
                .as_str()
                .expect("a group")
                .to_string()
        })
        .collect();
    let mut expected = groups.to_vec();
    expected.sort();
    assert_eq!(created, expected);
}
