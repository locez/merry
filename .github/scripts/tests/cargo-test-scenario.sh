#!/usr/bin/env bash
set -euo pipefail

helper=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)/cargo-test-scenario.sh
fixture_dir=$(mktemp -d)
trap 'rm -rf -- "$fixture_dir"' EXIT
export EXECUTION_LOG="$fixture_dir/executed"
export PATH="$fixture_dir:$PATH"

cat > "$fixture_dir/cargo" <<'CARGO'
#!/usr/bin/env bash
set -euo pipefail
if [[ "${!#}" == "--list" ]]; then
    case "$SCENARIO_FIXTURE" in
        empty) printf '0 tests, 0 benchmarks\n' ;;
        list_failure) exit 23 ;;
        many)
            for index in {1..5000}; do
                printf 'fixture::selected_scenario_%s: test\n' "$index"
            done
            ;;
        *) printf 'fixture::selected_scenario: test\n' ;;
    esac
else
    printf 'executed\n' >> "$EXECUTION_LOG"
    if [[ "$SCENARIO_FIXTURE" == "test_failure" ]]; then
        exit 29
    fi
fi
CARGO
chmod +x "$fixture_dir/cargo"

check_case() {
    export SCENARIO_FIXTURE="$1"
    local expected_status="$2"
    local expected_executions="$3"
    local actual_status=0
    : > "$EXECUTION_LOG"
    bash "$helper" -p fixture selected_scenario > "$fixture_dir/output" 2>&1 || actual_status=$?
    if [[ "$actual_status" != "$expected_status" ]]; then
        cat "$fixture_dir/output" >&2
        printf 'scenario %s: expected status %s, got %s\n' "$SCENARIO_FIXTURE" "$expected_status" "$actual_status" >&2
        exit 1
    fi
    local executions
    executions=$(wc -l < "$EXECUTION_LOG")
    if [[ "$executions" -ne "$expected_executions" ]]; then
        printf 'scenario %s: expected %s executions, got %s\n' "$SCENARIO_FIXTURE" "$expected_executions" "$executions" >&2
        exit 1
    fi
}

check_case match 0 1
check_case many 0 1
check_case empty 1 0
check_case list_failure 23 0
check_case test_failure 29 1
printf 'scenario selection: 5 cases passed\n'
