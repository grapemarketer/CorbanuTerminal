| pattern | 1000 median ms | 2000 median ms | 4000 median ms | 8000 median ms | doubling ratios | verdict |
|---|---|---|---|---|---|---|
| nested_substitution | 92.252 | 138.068 | 203.693 | 424.946 | 1.50, 1.48, 2.09 | linear |
| alias_chain | 131.083 | 197.656 | 279.915 | 492.406 | 1.51, 1.42, 1.76 | linear |
| arithmetic | 40.063 | 43.810 | 43.822 | 61.638 | 1.09, 1.00, 1.41 | linear |
| glob_alternatives | 58.826 | 93.187 | 158.734 | 263.658 | 1.58, 1.70, 1.66 | linear |
| nested_quotes | 37.162 | 48.086 | 63.539 | 61.125 | 1.29, 1.32, 0.96 | linear |

Verdicts on each line (classifier result):
- nested_substitution: Some(UnseenCode)
- alias_chain: Some(UnseenCode)
- arithmetic: None
- glob_alternatives: None, Some(UnseenCode)
- nested_quotes: Some(UnseenCode)
