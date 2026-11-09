| pattern | 1000 median ms | 2000 median ms | 4000 median ms | 8000 median ms | doubling ratios | verdict |
|---|---|---|---|---|---|---|
| nested_substitution | 33.746 | 67.954 | 136.238 | 278.834 | 2.01, 2.00, 2.05 | linear |
| alias_chain | 65.328 | 102.213 | 176.309 | 306.195 | 1.56, 1.72, 1.74 | linear |
| arithmetic | 1.429 | 2.758 | 5.455 | 11.080 | 1.93, 1.98, 2.03 | linear |
| glob_alternatives | 20.284 | 40.390 | 76.613 | 147.403 | 1.99, 1.90, 1.92 | linear |
| nested_quotes | 2.383 | 4.580 | 8.877 | 18.159 | 1.92, 1.94, 2.05 | linear |

Verdicts on each line (classifier result):
- nested_substitution: Some(UnseenCode)
- alias_chain: Some(UnseenCode)
- arithmetic: None
- glob_alternatives: None, Some(UnseenCode)
- nested_quotes: Some(UnseenCode)
