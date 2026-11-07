# Candidate identity (host, before staging)

```
$ shasum -a 256 corbanu.31b9f3ca
bbae7d37dff9023268efdd05a8a4408ccf144ee51ba911afd39adc0e9ddce1b8  corbanu.31b9f3ca
$ codesign -dv corbanu.31b9f3ca (selected lines)
Identifier=com.corbanu.corbanu
Format=Mach-O thin (arm64)
Timestamp=21 Oct 2026 at 12:55:07 pm
TeamIdentifier=K5N2T25C23
$ codesign --verify --strict corbanu.31b9f3ca
verify ok
$ git merge-base --is-ancestor 76d99da427 31b9f3cad8 (#1192) / d6c15422a9 31b9f3cad8 (#1250)
#1192 in 31b9f3cad8
#1250 in 31b9f3cad8
```

# Per-clone digests from each clone's provision.out (first 12/16 hex)

```
G1-20261021T223209Z corbanu=bbae7d37dff90232 agms-tui=54d2cb4f1112 agms-prep=d9cb0744bc2d agms-launch=2778aa90d324
G2-20261021T224529Z corbanu=bbae7d37dff90232 agms-tui=54d2cb4f1112 agms-prep=76f197a77989 agms-launch=7ade4dd0528a
G3-20261021T225952Z corbanu=bbae7d37dff90232 agms-tui=0be2fd2ed62e agms-prep=76f197a77989 agms-launch=7ade4dd0528a
G4-20261021T231430Z corbanu=bbae7d37dff90232 agms-tui=0be2fd2ed62e agms-prep=76f197a77989 agms-launch=7ade4dd0528a
G5-20261021T232839Z corbanu=bbae7d37dff90232 agms-tui=0be2fd2ed62e agms-prep=76f197a77989 agms-launch=7ade4dd0528a
G6-20261021T234437Z corbanu=bbae7d37dff90232 agms-tui=0be2fd2ed62e agms-prep=76f197a77989 agms-launch=7ade4dd0528a
G6R-20261022T005138Z corbanu=bbae7d37dff90232 agms-tui=8c99e064b9fe agms-prep=76f197a77989 agms-launch=7ade4dd0528a
G7-20261022T001545Z corbanu=bbae7d37dff90232 agms-tui=0be2fd2ed62e agms-prep=76f197a77989 agms-launch=7ade4dd0528a
G8-20261022T002932Z corbanu=bbae7d37dff90232 agms-tui=0be2fd2ed62e agms-prep=76f197a77989 agms-launch=7ade4dd0528a
G8R-20261022T010546Z corbanu=bbae7d37dff90232 agms-tui=8c99e064b9fe agms-prep=76f197a77989 agms-launch=7ade4dd0528a
G9-20261021T215855Z corbanu=bbae7d37dff90232 agms-tui=54d2cb4f1112 agms-prep=d9cb0744bc2d agms-launch=2778aa90d324
R1-20261022T011429Z corbanu=bbae7d37dff90232 agms-tui=8c99e064b9fe agms-prep=76f197a77989 agms-launch=7ade4dd0528a
R2-20261022T013047Z corbanu=bbae7d37dff90232 agms-tui=8c99e064b9fe agms-prep=76f197a77989 agms-launch=7ade4dd0528a
R3-20261022T014647Z corbanu=bbae7d37dff90232 agms-tui=8c99e064b9fe agms-prep=76f197a77989 agms-launch=7ade4dd0528a
R4-20261022T021152Z corbanu=bbae7d37dff90232 agms-tui=8c99e064b9fe agms-prep=76f197a77989 agms-launch=7ade4dd0528a
probe-na05-20261022T023953Z corbanu=bbae7d37dff90232 agms-tui=8c99e064b9fe agms-prep=76f197a77989 agms-launch=7ade4dd0528a
probe-na05-20261022T024612Z corbanu=bbae7d37dff90232 agms-tui=8c99e064b9fe agms-prep=76f197a77989 agms-launch=7ade4dd0528a
shakedown-20261021T215325Z corbanu=bbae7d37dff90232 agms-tui=54d2cb4f1112 agms-prep=d9cb0744bc2d agms-launch=2778aa90d324
shakedown1-prep-bug-20261021T214835Z corbanu=bbae7d37dff90232 agms-tui=54d2cb4f1112 agms-prep=d9cb0744bc2d agms-launch=99fd3dc6dea1
```

# Integrator probe arm B clones (added after review round 3)

```
probe-na05B-20261022T032245Z corbanu=bbae7d37dff90232 agms-tui=8c99e064b9fe agms-prep=76f197a77989 agms-launch=7ade4dd0528a
failed-norelay-probe-na05B-20261022T031535Z corbanu=bbae7d37dff90232 agms-tui=8c99e064b9fe agms-prep=76f197a77989 agms-launch=7ade4dd0528a
failed-stage-probeB-0315 (staging failed before provisioning; no digests)
```
