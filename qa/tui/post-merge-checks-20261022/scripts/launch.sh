#!/bin/bash
# usage: launch.sh <bin> <rundir> <zai|zai-agg|mock> [extra args...]
# Disposable HOME/CORBANU_HOME, no native keyring. The Z.AI key comes from the
# installed vault helper (label zai_key_3) on the consuming command only; it is
# never echoed, written to a file, or traced.
BIN=$1; R=$2; MODE=$3; shift 3
REAL_HOME=$HOME
LAUNCHER=$REAL_HOME/.local/bin/corbanu
mkdir -p "$R/home" "$R/ws" "$R/userhome" "$R/logs"
if [ ! -d "$R/ws/.git" ]; then
  (cd "$R/ws" && git init -q . && printf 'buy coffee\n' > notes.txt)
fi
if [ ! -f "$R/home/config.toml" ]; then
  {
    echo 'suppress_unstable_features_warning = true'
    [ -n "$SKIP_REOPEN" ] && echo 'skip_reopen_warmup = true'
    if [ "$MODE" = mock ]; then
      echo 'model = "mock-ws-1"'
      echo 'model_provider = "mockws"'
    fi
    if [ "$MODE" = zai-agg ]; then
      cat <<'F'
[features]
security_levels = true
protected_mode_preflight = true
secretless_agent_launch = true
isolated_credential_broker = true
secret_output_gate = true
source_envelopes = true
url_destination_policy = true
contained_external_agents = true
F
    fi
    if [ "$MODE" = mock ]; then
      cat <<F
[model_providers.mockws]
name = "MockWS"
base_url = "http://127.0.0.1:${MOCK_PORT:-18791}/v1"
wire_api = "responses"
supports_websockets = true
requires_openai_auth = false
F
    fi
    echo "[projects.\"$R/ws\"]"
    echo 'trust_level = "trusted"'
  } > "$R/home/config.toml"
fi
for v in $(compgen -e); do case "$v" in *API_KEY*|*TOKEN*|*SECRET*|*PASSWORD*|*CREDENTIAL*|*_AUTH*) unset "$v";; esac; done
export HOME=$R/userhome CODEX_HOME=$R/home CORBANU_HOME=$R/home PFTERMINAL_HOME=$R/home CORBANU_TEST_NO_NATIVE_KEYRING=1 TERM=xterm-256color
cd "$R/ws"
if [ "$MODE" = mock ]; then
  exec "$BIN" -C "$R/ws" -c log_dir=\"$R/logs\" "$@"
fi
ZAI_API_KEY="$(env -u CORBANU_TEST_NO_NATIVE_KEYRING -u CORBANU_HOME -u CODEX_HOME -u PFTERMINAL_HOME HOME="$REAL_HOME" "$LAUNCHER" vault auth-helper zai_key_3)" exec "$BIN" -C "$R/ws" -c log_dir=\"$R/logs\" -m glm-5.3-flash -c model_provider=\"zai\" "$@"
