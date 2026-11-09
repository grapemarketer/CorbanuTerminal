| pattern | 1000 median ms | 2000 median ms | 4000 median ms | 8000 median ms | doubling ratios | verdict |
|---|---|---|---|---|---|---|
| nested_substitution | 26.252 | 52.659 | 107.037 | 218.171 | 2.01, 2.03, 2.04 | linear |
| alias_chain | 45.517 | 74.504 | 132.289 | 233.176 | 1.64, 1.78, 1.76 | linear |
| arithmetic | 1.148 | 2.252 | 4.529 | 9.267 | 1.96, 2.01, 2.05 | linear |
| glob_alternatives | 15.189 | 30.945 | 57.321 | 111.186 | 2.04, 1.85, 1.94 | linear |
| nested_quotes | 1.706 | 3.230 | 6.283 | 12.919 | 1.89, 1.95, 2.06 | linear |

Verdicts on each line (classifier result):
- nested_substitution: Some(UnseenCode)
- alias_chain: Some(UnseenCode)
- arithmetic: None
- glob_alternatives: None, Some(UnseenCode)
- nested_quotes: Some(UnseenCode)
