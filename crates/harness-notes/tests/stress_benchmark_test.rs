use harness_core::*;
use harness_notes::*;
use std::time::Instant;

#[tokio::test]
async fn test_stress_chunking_and_reconciliation_benchmark() {
    let engine = FileNotesEngine::in_memory().unwrap();

    // 1. Benchmark: Large document chunking (128 KB document -> ~20 chunks)
    println!("\n=== BENCHMARK 1: NOTE CHUNKING & RECONSTRUCTION ===");
    let paragraph = "Luna autonomous AI systems engineer persistent memory note chunking verification test line.\n";
    let large_body = paragraph.repeat(1500); // ~135 KB
    assert!(large_body.len() > 120_000);

    let start = Instant::now();
    let saved = engine
        .save_note("Large Specification Document", &large_body, &["benchmark".to_string()])
        .await
        .unwrap();
    let save_duration = start.elapsed();
    println!("Saved 135 KB note in: {:?}", save_duration);

    // Verify chunks exist on disk
    let files = std::fs::read_dir(engine.dir().join("inbox").join("notes")).unwrap();
    let part_files: Vec<_> = files
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().contains("-part-"))
        .collect();
    println!("Created {} chunk parts on disk", part_files.len());
    assert!(part_files.len() >= 15);

    // Read back and reconstruct
    let start_read = Instant::now();
    let read_back = engine.read_note(&saved.id).await.unwrap();
    let read_duration = start_read.elapsed();
    println!("Read back and reconstructed 135 KB note in: {:?}", read_duration);
    assert_eq!(read_back.content.len(), large_body.trim().len());
    assert_eq!(read_back.content, large_body.trim());

    // 2. Benchmark: Rapid near-match reconciliation (500 sequential updates)
    println!("\n=== BENCHMARK 2: RAPID NEAR-MATCH RECONCILIATION ===");
    let base_title = "Active Deployment Procedures 2026";
    let mut last_note = engine
        .save_note(base_title, "Initial base procedure.", &["deploy".to_string()])
        .await
        .unwrap();

    let count = 200;
    let start_recon = Instant::now();
    for i in 1..=count {
        let variant_title = if i % 2 == 0 {
            "Active Deployment Procedures"
        } else {
            "Active Deployment Procedures 2026"
        };
        let new_step = format!("Step {}: verified at milestone {}.", i, i);
        last_note = engine
            .save_note(variant_title, &new_step, &["deploy".to_string()])
            .await
            .unwrap();
    }
    let recon_duration = start_recon.elapsed();
    println!(
        "Processed {} reconciliations in: {:?} ({:.2} ops/sec, avg {:.2} ms/op)",
        count,
        recon_duration,
        count as f64 / recon_duration.as_secs_f64(),
        (recon_duration.as_secs_f64() * 1000.0) / count as f64
    );
    assert_eq!(last_note.id, "active-deployment-procedures-2026");
    assert!(last_note.content.contains("Step 200:"));

    // 3. Benchmark: FTS Search across notes
    println!("\n=== BENCHMARK 3: FTS SEARCH SPEED ===");
    let start_search = Instant::now();
    let searches = 1000;
    for _ in 0..searches {
        let res = engine.search_notes("verified milestone", 5).await.unwrap();
        assert!(!res.is_empty());
    }
    let search_duration = start_search.elapsed();
    println!(
        "Executed {} searches in: {:?} ({:.2} searches/sec, avg {:.3} ms/search)",
        searches,
        search_duration,
        searches as f64 / search_duration.as_secs_f64(),
        (search_duration.as_secs_f64() * 1000.0) / searches as f64
    );

    println!("\n=== BENCHMARK COMPLETE: ALL CHECKS PASSED ===\n");
}
