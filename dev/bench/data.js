window.BENCHMARK_DATA = {
  "lastUpdate": 1767527851285,
  "repoUrl": "https://github.com/olegmukhin/thoughtgate",
  "entries": {
    "Benchmark": [
      {
        "commit": {
          "author": {
            "email": "oleg.v.mukhin@gmail.com",
            "name": "Oleg Mukhin",
            "username": "olegmukhin"
          },
          "committer": {
            "email": "oleg.v.mukhin@gmail.com",
            "name": "Oleg Mukhin",
            "username": "olegmukhin"
          },
          "distinct": true,
          "id": "6e84ea0b390d6248f735e1ac4441aaf77c024fec",
          "message": "fix: header redaction security bugs and benchmark CI initialization\n\nSecurity Fixes (Found by Fuzzer):\n- Fix case-sensitive header comparison allowing credential leaks\n  Headers like \"Cookie\", \"COOKIE\", \"Authorization\" were not being\n  redacted due to case-sensitive string matching. HTTP header names\n  are case-insensitive per RFC 7230 Section 3.2.\n\n- Fix memory allocation crash in header sanitization\n  The .to_lowercase() approach allocated a String for every header,\n  causing crashes with malformed input and potential DoS via memory\n  exhaustion.\n\n- Solution: Use zero-allocation .eq_ignore_ascii_case() for header\n  comparison. This is faster, safer, and prevents both bugs.\n\nCI Fix:\n- Add automatic gh-pages branch creation for benchmarks\n  The benchmark-action failed on first run because gh-pages branch\n  didn't exist. Added setup step to create orphan branch with README\n  if needed, allowing benchmark tracking to work from first run.\n\nDiscovered-by: cargo-fuzz (fuzz_header_redaction target)",
          "timestamp": "2026-01-04T10:19:20Z",
          "tree_id": "3289b7e920aee5073d2d85a594c624f210fd3747",
          "url": "https://github.com/olegmukhin/thoughtgate/commit/6e84ea0b390d6248f735e1ac4441aaf77c024fec"
        },
        "date": 1767522118637,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ttfb/direct/baseline",
            "value": 131606.40457170393,
            "unit": "ns"
          },
          {
            "name": "ttfb/proxied/with_relay",
            "value": 11398889.061111115,
            "unit": "ns"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "oleg.v.mukhin@gmail.com",
            "name": "Oleg Mukhin",
            "username": "olegmukhin"
          },
          "committer": {
            "email": "oleg.v.mukhin@gmail.com",
            "name": "Oleg Mukhin",
            "username": "olegmukhin"
          },
          "distinct": true,
          "id": "ce3c78ca992b2e7e5fd3be8c77958bea1fd23200",
          "message": "Formatting fixes",
          "timestamp": "2026-01-04T10:34:42Z",
          "tree_id": "83cbecd8d145870c3f49bff2d51c3fb5f2fcad8c",
          "url": "https://github.com/olegmukhin/thoughtgate/commit/ce3c78ca992b2e7e5fd3be8c77958bea1fd23200"
        },
        "date": 1767523021118,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ttfb/direct/baseline",
            "value": 95455.66009773948,
            "unit": "ns"
          },
          {
            "name": "ttfb/proxied/with_relay",
            "value": 11379650.08333333,
            "unit": "ns"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "oleg.v.mukhin@gmail.com",
            "name": "Oleg Mukhin",
            "username": "olegmukhin"
          },
          "committer": {
            "email": "oleg.v.mukhin@gmail.com",
            "name": "Oleg Mukhin",
            "username": "olegmukhin"
          },
          "distinct": true,
          "id": "33bdeaf5a2ef7a6cbc70c01fa561534db59c5056",
          "message": "refactor: streamline header sanitization logic in fuzz test\n\n- Consolidated header name and value creation to reduce redundancy.\n- Enhanced sensitive header detection to ensure proper redaction patterns are checked.\n- Improved assertions for verifying that sensitive headers are not leaked and are correctly redacted.\n\nThis refactor aims to improve code clarity and maintainability while ensuring robust security checks for sensitive headers.",
          "timestamp": "2026-01-04T11:39:37Z",
          "tree_id": "42297936c89bf09d3554d3f0cf1b13bf4793e710",
          "url": "https://github.com/olegmukhin/thoughtgate/commit/33bdeaf5a2ef7a6cbc70c01fa561534db59c5056"
        },
        "date": 1767526911878,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ttfb/direct/baseline",
            "value": 128714.0174946416,
            "unit": "ns"
          },
          {
            "name": "ttfb/proxied/with_relay",
            "value": 11366306.38,
            "unit": "ns"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "oleg.v.mukhin@gmail.com",
            "name": "Oleg Mukhin",
            "username": "olegmukhin"
          },
          "committer": {
            "email": "oleg.v.mukhin@gmail.com",
            "name": "Oleg Mukhin",
            "username": "olegmukhin"
          },
          "distinct": true,
          "id": "ae76348d58c6a43f2a13766990e6594e2b777eb7",
          "message": "chore: update dependencies and remove unused packages\n\n- Removed obsolete dependencies from Cargo.lock and Cargo.toml, including aws-lc-rs, aws-lc-sys, cmake, dunce, fs_extra, and jobserver.\n- Updated hyper-rustls and rustls configurations to disable default features and include specific features for improved performance and security.\n\nThese changes help streamline the dependency tree and enhance the overall project configuration.",
          "timestamp": "2026-01-04T11:54:38Z",
          "tree_id": "4634c40fb0ca14e25073af9201a98680a546e9a3",
          "url": "https://github.com/olegmukhin/thoughtgate/commit/ae76348d58c6a43f2a13766990e6594e2b777eb7"
        },
        "date": 1767527850883,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "ttfb/direct/baseline",
            "value": 139242.0134312134,
            "unit": "ns"
          },
          {
            "name": "ttfb/proxied/with_relay",
            "value": 11345661.565555556,
            "unit": "ns"
          }
        ]
      }
    ]
  }
}