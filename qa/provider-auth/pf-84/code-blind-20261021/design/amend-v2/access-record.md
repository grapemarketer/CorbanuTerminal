# Access record (amendment v2)

## designer
```
/bin/bash -lc 'ls -la; ls screens; wc -c *.md screens/*' in <Q>/design/amend-v2/packet
/bin/bash -lc 'cat intent.md tools.md review-facts.md run-observations.md' in <Q>/design/amend-v2/packet
/bin/bash -lc 'cat design-cases.md' in <Q>/design/amend-v2/packet
/bin/bash -lc "sed -n 330,400p design-cases.md; sed -n '/"'^## Open questions/,$p'"' design-cases.md | head -80; grep -n \""'^##'"\\|"'^###" design-cases.md' in <Q>/design/amend-v2/packet
/bin/bash -lc 'cat provider-accounts-doc.md' in <Q>/design/amend-v2/packet
/bin/bash -lc 'cd screens; for f in *; do echo "=== $f"; cat $f; done' in <Q>/design/amend-v2/packet
```

## review
```
/bin/bash -lc 'ls -la; wc -c *.md screens/*.txt' in <Q>/design/amend-v2/review-packet
/bin/bash -lc 'cat intent.md tools.md review-facts.md run-observations.md' in <Q>/design/amend-v2/review-packet
/bin/bash -lc 'cat design-cases.md' in <Q>/design/amend-v2/review-packet
/bin/bash -lc 'sed -n 300,380p design-cases.md' in <Q>/design/amend-v2/review-packet
/bin/bash -lc 'cat proposed-amendment.md' in <Q>/design/amend-v2/review-packet
/bin/bash -lc 'cat provider-accounts-doc.md' in <Q>/design/amend-v2/review-packet
/bin/bash -lc 'cd screens; for f in *.txt; do echo "=== $f"; cat $f; done' in <Q>/design/amend-v2/review-packet
```
