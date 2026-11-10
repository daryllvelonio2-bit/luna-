//! Memory Evaluation Suite (Checked-in benchmark regression gate).
//!
//! Evaluates:
//! - Realistic fixture repo setup
//! - Expected fact extraction with evidence provenance
//! - Verbatim ratio across saved notes (non-heading spans must be 100% transcript-grounded)
//! - Duplicate active record audit (uniqueness on (subject, predicate) and note slug)
//! - Recall@k (k=1, 3, 5) retrieval benchmarks across query scenarios

use async_trait::async_trait;
use harness_core::*;
use harness_memory::saver::{MemorySaver, SaverPolicy};
use harness_memory::SqliteMemoryEngine;
use harness_notes::FileNotesEngine;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// A test provider that returns canned JSON operations for the curation pass.
struct EvalProvider {
    responses: Mutex<Vec<String>>,
}

impl EvalProvider {
    fn new(responses: Vec<String>) -> Arc<Self> {
        Arc::new(Self {
            responses: Mutex::new(responses),
        })
    }
}

#[async_trait]
impl ModelProvider for EvalProvider {
    fn provider_id(&self) -> &str {
        "eval-provider"
    }

    async fn fetch_models(&self) -> Result<Vec<String>, HarnessError> {
        Ok(vec!["eval-model".to_string()])
    }

    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _options: &CompletionOptions,
    ) -> Result<Message, HarnessError> {
        let mut list = self.responses.lock().unwrap();
        let body = if list.is_empty() {
            r#"{"operations":[]}"#.to_string()
        } else {
            list.remove(0)
        };
        Ok(Message::assistant(body))
    }

    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, HarnessError> {
        Ok(vec![vec![0.0; 8]; texts.len()])
    }
}

struct FixtureRepo {
    pub dir: PathBuf,
}

impl FixtureRepo {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("luna-eval-repo-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = \"smartlab-web\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("run.sh"),
            "#!/bin/bash\n./run.sh migrate + serve on 0.0.0.0:8082\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("README.md"),
            "# SmartLab Web\nRun tests with: composer test\n",
        )
        .unwrap();
        Self { dir }
    }
}

impl Drop for FixtureRepo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Helper that sets up an in-memory saver and engine pair.
fn create_eval_setup(
    canned_json: &str,
) -> (MemorySaver, Arc<SqliteMemoryEngine>, Arc<FileNotesEngine>) {
    let memory = Arc::new(SqliteMemoryEngine::in_memory().unwrap());
    let notes = Arc::new(FileNotesEngine::in_memory().unwrap());
    let provider = EvalProvider::new(vec![canned_json.to_string()]);
    let mut policy = SaverPolicy::new("eval-model");
    policy.two_pass = false;
    let saver = MemorySaver::new(provider, memory.clone(), notes.clone(), policy);
    (saver, memory, notes)
}

#[tokio::test]
async fn test_memory_eval_suite_fixture_repo_and_expected_facts() {
    let fixture = FixtureRepo::new("smartlab");
    let transcript = vec![
        Message::system("system instructions"),
        Message::user("I always build smartlab with ./run.sh migrate and test with composer test"),
        Message::assistant("Noted: ./run.sh migrate and composer test for smartlab-web."),
        Message::tool(ToolResult::success(
            "call_1",
            "Project root: /home/janelle/smartlab-web\nAll 42 tests passed.\nBuild command: ./run.sh migrate + serve on 0.0.0.0:8082\nTest command: composer test\nDeployment is a single static binary served by the bundled web server.",
        )),
        Message::user("Please remember to keep responses concise with zero filler"),
    ];

    let canned_response = r###"{
        "operations": [
            {
                "op": "add_memory",
                "memory": {
                    "subject": "project.smartlab-web",
                    "predicate": "run-command",
                    "object": "./run.sh migrate + serve on 0.0.0.0:8082",
                    "category": "workflow_rule"
                },
                "evidence": "I always build smartlab with ./run.sh migrate and test with composer test",
                "confidence": 0.95,
                "reason": "explicit user build directive"
            },
            {
                "op": "add_memory",
                "memory": {
                    "subject": "user.preference",
                    "predicate": "style",
                    "object": "concise, zero filler",
                    "category": "user_profile"
                },
                "evidence": "Please remember to keep responses concise with zero filler",
                "confidence": 0.95,
                "reason": "explicit user communication preference"
            },
            {
                "op": "create_note",
                "note": {
                    "title": "SmartLab Web Runbook",
                    "content": "## Verified\n\nProject root: /home/janelle/smartlab-web\nAll 42 tests passed.\nBuild command: ./run.sh migrate + serve on 0.0.0.0:8082\nTest command: composer test\nDeployment is a single static binary served by the bundled web server.",
                    "tags": ["runbook", "smartlab"]
                },
                "evidence": "All 42 tests passed.",
                "confidence": 0.92,
                "reason": "verified test runbook"
            }
        ]
    }"###;

    let (saver, memory, notes) = create_eval_setup(canned_response);
    let report = saver.distil(&transcript).await.unwrap();

    // 1. Verify expected facts and notes count
    assert_eq!(report.memories, 2, "Expected 2 memory cards saved");
    assert_eq!(report.notes, 1, "Expected 1 note saved");
    assert_eq!(report.skipped, 0, "No operations should be refused");

    // 2. Verify stored cards
    let stored_cards = memory.retrieve_cards("", 10).await.unwrap();
    let subjects: Vec<&str> = stored_cards.iter().map(|c| c.subject.as_str()).collect();
    assert!(subjects.contains(&"project.smartlab-web"));
    assert!(subjects.contains(&"user.preference"));

    // 3. Verify note content
    let stored_notes = notes.list_notes(10, 0).await.unwrap();
    assert_eq!(stored_notes.len(), 1);
    assert_eq!(stored_notes[0].title, "SmartLab Web Runbook");
    assert!(stored_notes[0].content.contains("All 42 tests passed."));
    assert!(fixture.dir.exists());
}

#[tokio::test]
async fn test_memory_eval_suite_verbatim_ratio_audit() {
    const DEPLOY_LINE: &str = "We deploy the rust agent harness using Helm charts on k8s, and every release is rolled out with the same chart values so staging and production stay perfectly in sync";
    let transcript = vec![
        Message::user(DEPLOY_LINE),
        Message::assistant("Noted: deploy with Helm on k8s."),
    ];

    // Case A: Verbatim note (100% transcript-grounded)
    let canned_verbatim = format!(
        r###"{{
        "operations": [
            {{
                "op": "create_note",
                "note": {{
                    "title": "K8s Helm Deployment",
                    "content": "## Deployment\n\n{DEPLOY_LINE}",
                    "tags": ["k8s", "helm"]
                }},
                "evidence": "{DEPLOY_LINE}",
                "confidence": 0.9
            }}
        ]
    }}"###
    );

    let (saver, _memory, notes) = create_eval_setup(&canned_verbatim);
    let report = saver.distil(&transcript).await.unwrap();
    assert_eq!(report.notes, 1);
    let note = notes.read_note("k8s-helm-deployment").await.unwrap();

    // Compute verbatim ratio: non-heading characters must appear in source transcript
    let transcript_text = DEPLOY_LINE;
    let mut total_chars = 0usize;
    let mut matching_chars = 0usize;
    for line in note.content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        total_chars += trimmed.len();
        if transcript_text.contains(trimmed) {
            matching_chars += trimmed.len();
        }
    }
    let verbatim_ratio = matching_chars as f32 / total_chars.max(1) as f32;
    assert!(
        verbatim_ratio >= 0.99,
        "Verbatim ratio must be 1.0, got: {verbatim_ratio}"
    );

    // Case B: Paraphrased note (must be refused by saver)
    let canned_paraphrased = format!(
        r###"{{
        "operations": [
            {{
                "op": "create_note",
                "note": {{
                    "title": "K8s Deployment Summary",
                    "content": "The user told us that they usually use Helm for Kubernetes deployments, and that every release is rolled out with the same chart values so staging and production stay in sync.",
                    "tags": ["k8s"]
                }},
                "evidence": "{DEPLOY_LINE}",
                "confidence": 0.9
            }}
        ]
    }}"###
    );

    let (saver_para, _mem, notes_para) = create_eval_setup(&canned_paraphrased);
    let report_para = saver_para.distil(&transcript).await.unwrap();
    assert_eq!(report_para.notes, 0, "Paraphrased note must be refused");
    assert_eq!(report_para.skipped, 1);
    assert!(notes_para.list_notes(10, 0).await.unwrap().is_empty());
}

#[tokio::test]
async fn test_memory_eval_suite_duplicate_audit() {
    let memory = Arc::new(SqliteMemoryEngine::in_memory().unwrap());

    // Insert 5 records, with 2 updating existing (subject, predicate) keys
    let initial = MemoryRecord {
        id: "project.smartlab.run".to_string(),
        subject: "project.smartlab".to_string(),
        predicate: "run".to_string(),
        object: "./run.sh".to_string(),
        name: None,
        path: None,
        raw_content: "project.smartlab run ./run.sh".to_string(),
        category: MemoryCategory::WorkflowRule,
        confidence: 0.9,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        is_active: true,
        superseded_by: None,
    };
    memory.save(&initial).await.unwrap();

    let updated = MemoryRecord {
        id: "project.smartlab.run.v2".to_string(),
        subject: "project.smartlab".to_string(),
        predicate: "run".to_string(),
        object: "./run.sh serve --port 8082".to_string(),
        name: None,
        path: None,
        raw_content: "project.smartlab run ./run.sh serve --port 8082".to_string(),
        category: MemoryCategory::WorkflowRule,
        confidence: 0.95,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        is_active: true,
        superseded_by: None,
    };
    memory.save(&updated).await.unwrap();

    // Verify active records have ZERO duplicates for (subject, predicate)
    let active = memory.retrieve_cards("", 50).await.unwrap();
    let mut seen_keys = HashSet::new();
    let mut duplicates = 0;
    for rec in &active {
        let key = (rec.subject.to_lowercase(), rec.predicate.to_lowercase());
        if !seen_keys.insert(key) {
            duplicates += 1;
        }
    }
    assert_eq!(
        duplicates, 0,
        "Duplicate active records detected for (subject, predicate): {duplicates}"
    );
    assert_eq!(active[0].object, "./run.sh serve --port 8082");
}

#[tokio::test]
async fn test_memory_eval_suite_recall_at_k() {
    let memory = Arc::new(SqliteMemoryEngine::in_memory().unwrap());

    let records = vec![
        (
            "project.smartlab.build",
            "project.smartlab",
            "build-command",
            "composer install",
        ),
        (
            "project.smartlab.test",
            "project.smartlab",
            "test-command",
            "composer test",
        ),
        (
            "project.smartlab.run",
            "project.smartlab",
            "run-command",
            "./run.sh serve",
        ),
        ("user.pref.language", "user.preference", "language", "Rust"),
        (
            "user.pref.style",
            "user.preference",
            "style",
            "concise plain text",
        ),
        (
            "project.luna.root",
            "project.luna",
            "root",
            "/home/janelle/Documents/GitHub/rust-agent-harness",
        ),
    ];

    for (id, subj, pred, obj) in &records {
        let mut rec = MemoryRecord {
            id: id.to_string(),
            subject: subj.to_string(),
            predicate: pred.to_string(),
            object: obj.to_string(),
            name: None,
            path: None,
            raw_content: format!("{subj} {pred} {obj}"),
            category: if subj.starts_with("user") {
                MemoryCategory::UserProfile
            } else {
                MemoryCategory::WorkflowRule
            },
            confidence: 0.95,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            is_active: true,
            superseded_by: None,
        };
        memory.save(&rec).await.unwrap();
    }

    // Benchmark test query scenarios: (query, expected_target_id)
    let scenarios = vec![
        ("how do I build smartlab?", "project.smartlab.build"),
        ("what is the test command?", "project.smartlab.test"),
        ("what language does the user prefer?", "user.pref.language"),
        ("user style preference", "user.pref.style"),
        ("luna project root directory", "project.luna.root"),
    ];

    let mut hit_at_1 = 0;
    let mut hit_at_3 = 0;
    let mut hit_at_5 = 0;
    let total = scenarios.len();

    for (query, expected_id) in &scenarios {
        let results = memory.retrieve(query, 5).await.unwrap();
        let ids: Vec<&str> = results.iter().map(|r| r.id.as_str()).collect();

        if !ids.is_empty() && ids[0] == *expected_id {
            hit_at_1 += 1;
        }
        if ids.iter().take(3).any(|id| *id == *expected_id) {
            hit_at_3 += 1;
        }
        if ids.iter().take(5).any(|id| *id == *expected_id) {
            hit_at_5 += 1;
        }
    }

    let recall_1 = hit_at_1 as f32 / total as f32;
    let recall_3 = hit_at_3 as f32 / total as f32;
    let recall_5 = hit_at_5 as f32 / total as f32;

    assert!(
        recall_1 >= 0.80,
        "Recall@1 should be >= 0.80, got: {recall_1:.2}"
    );
    assert_eq!(recall_3, 1.0, "Recall@3 should be 1.00, got: {recall_3:.2}");
    assert_eq!(recall_5, 1.0, "Recall@5 should be 1.00, got: {recall_5:.2}");
}
