# Host Z.AI relay (how it was started)

In the run's private tmux server, one window, the key fetched at use time and never printed:

    ZAI_RELAY_KEY="$(corbanu vault auth-helper zai_key_3)" exec /usr/bin/python3 <Q>/h/host_zai_relay.py --port 11247 --log <Q>/logs/host-zai-relay.jsonl

Started 08:35:08Z, stopped after S3R (09:41Z). The guest reaches it only through the SSH reverse tunnel (18601).
`host_zai_relay.py` and `host_passthrough.py` are unchanged from the first run's harness/host/.
