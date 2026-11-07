# Mediation CA re-issue (acct-rerun2, 2026-10-27T17:51Z)
Clones A-R1-20261027T1744Z and C-R1-20261027T1748Z failed the gate (no group ran, no tester spend): the synthetic
mediation CA and relay leaf from acc-tart1 (ca/, h/mediation-ca.pem) expired 2026-10-27T01:48:51Z; the guest mediator
reported SSLV3_ALERT_CERTIFICATE_EXPIRED and lane probes saw EPERM instead of 403.
Fix: same CA key and leaf key/CSR/extensions, new validity (CA 14 d, leaf 13 d), CA subject renamed
"acct-rerun mediation CA (synthetic, reissued 2026-10-27)". Expired certificates kept in ca/expired-20261027/.
No design, packet, package or expected.json change (the amendment freeze c03044a2... is untouched).
