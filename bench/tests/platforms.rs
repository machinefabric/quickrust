//! qrust-bench on a platform it does not measure on.

#![cfg(not(any(target_os = "linux", target_os = "macos")))]

// TEST0003: On a platform it does not measure on, qrust-bench refuses and names the platforms it measures on
#[test]
fn test0003_on_a_platform_it_does_not_measure_on_qrust_bench_refuses_and_names_the_platforms_it_measures_on() {
    for args in [&["run", "--quick"][..], &[][..]] {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_qrust-bench")).args(args).output().unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(1), "{:?}: {}", args, stderr);
        assert_eq!(stderr.trim_end(), format!("FAIL qrust-bench measures on Linux and macOS only: it cannot measure on {}", std::env::consts::OS), "{:?}", args);
        assert!(out.stdout.is_empty(), "{:?}", args);
    }
}
