H="$AGMS_RUN/product-home"
cp "$H/config.toml" "$AGMS_RUN/evidence/config-before-remove-$(date -u +%Y%m%dT%H%M%SZ).toml"
awk '/^[[:space:]]*\[/ { skip = ($0 ~ /^[[:space:]]*\[agents\.p20wide\][[:space:]]*$/) } !skip' "$H/config.toml" > "$H/config.toml.new" && cat "$H/config.toml.new" > "$H/config.toml" && rm -f "$H/config.toml.new" "$H/agms-roles/p20wide.toml"
grep -c 'p20wide' "$H/config.toml"
grep -c '^\[agents\.p20plain\]' "$H/config.toml"
printf '%s\n' 'RECORD-ROLES'
grep -n -A2 '^\[agents\.' "$AGMS_RUN/product-home/config.toml"
for f in "$AGMS_RUN"/product-home/agms-roles/*.toml; do echo "== $f"; cat "$f"; shasum -a 256 "$f"; done
