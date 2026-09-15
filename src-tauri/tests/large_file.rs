//! Large files, slow writes, and locks.
//!
//! A 100 MB file does not appear; it arrives, over seconds. The watcher sees
//! it long before it is finished, and the difference between a backup client
//! and a broken one is what happens in between.
//!
//! The failure being guarded against is not a crash. It is uploading a
//! truncated file and *marking it synced* — the server then holds a corrupt
//! copy and nothing ever revisits it, because as far as the client is
//! concerned that file is done.

use std::io::Write;
use std::time::{Duration, Instant};

use arciin_desktop_lib::backup::engine::still_being_written;

/// 100 MB, written in chunks, as a download or an export would arrive.
const LARGE: usize = 100 * 1024 * 1024;
const CHUNK: usize = 2 * 1024 * 1024;

#[test]
fn a_file_being_written_right_now_is_not_ready_to_send() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("downloading.bin");

    let writer_path = path.clone();
    let writing = std::thread::spawn(move || {
        let mut file = std::fs::File::create(&writer_path).unwrap();
        for _ in 0..20 {
            file.write_all(&vec![0u8; CHUNK]).unwrap();
            file.flush().unwrap();
            std::thread::sleep(Duration::from_millis(120));
        }
    });

    // Sample while it is unmistakably still growing.
    std::thread::sleep(Duration::from_millis(300));
    let verdict = still_being_written(&path);
    assert!(
        verdict.is_some(),
        "a file that is actively growing must not be considered ready"
    );

    writing.join().unwrap();
}

#[test]
fn a_file_that_has_finished_is_ready_to_send() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("finished.bin");
    std::fs::write(&path, vec![0u8; CHUNK]).unwrap();
    // Let the write settle, as it would have by the time the debounce window
    // closed in the real path.
    std::thread::sleep(Duration::from_millis(600));

    assert!(
        still_being_written(&path).is_none(),
        "a finished file must not be deferred forever"
    );
}

#[test]
fn an_old_file_is_not_delayed_by_the_check() {
    // The check costs a short sleep. Paying it on every entry of a large
    // backup would add hours; only recently-touched files are worth sampling.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("old.bin");
    std::fs::write(&path, b"x").unwrap();

    // Backdate it well past the recency window.
    let long_ago = std::time::SystemTime::now() - Duration::from_secs(600);
    filetime::set_file_mtime(&path, filetime::FileTime::from_system_time(long_ago)).unwrap();

    let started = Instant::now();
    assert!(still_being_written(&path).is_none());
    assert!(
        started.elapsed() < Duration::from_millis(200),
        "an old file must not pay the settle delay"
    );
}

#[test]
fn a_file_rewritten_in_place_is_noticed_even_at_the_same_length() {
    // Length alone is not enough: a file rewritten in place keeps its size and
    // changes only its timestamp, and sending it mid-rewrite is the same bug.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("inplace.bin");
    std::fs::write(&path, vec![1u8; CHUNK]).unwrap();
    std::thread::sleep(Duration::from_millis(600));
    assert!(still_being_written(&path).is_none());

    let writer_path = path.clone();
    let writing = std::thread::spawn(move || {
        for _ in 0..8 {
            std::fs::write(&writer_path, vec![2u8; CHUNK]).unwrap();
            std::thread::sleep(Duration::from_millis(100));
        }
    });
    std::thread::sleep(Duration::from_millis(150));
    assert!(
        still_being_written(&path).is_some(),
        "a rewrite in place must be noticed"
    );
    writing.join().unwrap();
}

#[test]
fn checking_a_file_never_stops_anyone_writing_to_it() {
    // This check runs on files somebody is in the middle of saving. If it took
    // an exclusive handle it would make the save fail — turning the backup
    // client into the reason a document cannot be written.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("busy.bin");
    let mut file = std::fs::File::create(&path).unwrap();
    file.write_all(&vec![0u8; CHUNK]).unwrap();
    file.flush().unwrap();

    let _ = still_being_written(&path);

    // The handle is still open and must still work.
    file.write_all(&vec![0u8; CHUNK])
        .expect("the writer must not be locked out");
    file.flush().unwrap();
    drop(file);
    std::fs::remove_file(&path).expect("and the file must still be deletable");
}

#[test]
fn a_hundred_megabyte_file_settles_and_is_sent_once() {
    // The whole arc, at the size that matters: written gradually, sampled
    // while growing, then ready exactly once it stops.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big.bin");

    let writer_path = path.clone();
    let started = Instant::now();
    let writing = std::thread::spawn(move || {
        let mut file = std::fs::File::create(&writer_path).unwrap();
        let mut written = 0usize;
        while written < LARGE {
            file.write_all(&vec![7u8; CHUNK]).unwrap();
            file.flush().unwrap();
            written += CHUNK;
            std::thread::sleep(Duration::from_millis(60));
        }
    });

    // Sample repeatedly while it grows. Every sample must refuse.
    let mut refusals = 0;
    for _ in 0..4 {
        std::thread::sleep(Duration::from_millis(200));
        if still_being_written(&path).is_some() {
            refusals += 1;
        }
    }
    assert!(refusals > 0, "a growing file must be refused at least once");

    writing.join().unwrap();
    let write_time = started.elapsed();

    // Once finished it must become ready promptly, not be deferred forever.
    let settle_started = Instant::now();
    let mut ready = false;
    for _ in 0..20 {
        if still_being_written(&path).is_none() {
            ready = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(ready, "a finished file must become ready");

    assert_eq!(std::fs::metadata(&path).unwrap().len() as usize, LARGE);
    eprintln!(
        "100 MB written in {:?}; ready {:?} after the last byte; {refusals} refusals while growing",
        write_time,
        settle_started.elapsed()
    );
}
