window.BENCHMARK_DATA = {
  "lastUpdate": 1767523021861,
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
      }
    ]
  }
}