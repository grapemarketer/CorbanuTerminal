| pattern | 64 median ms | 128 median ms | 256 median ms | 512 median ms | doubling ratios | verdict |
|---|---|---|---|---|---|---|
| nested_substitution | 2.793 | 5.196 | 10.133 | 19.244 | 1.86, 1.95, 1.90 | linear |
| alias_chain | 8.366 | 16.561 | 32.602 | 43.612 | 1.98, 1.97, 1.34 | linear |
| arithmetic | 0.422 | 0.609 | 1.020 | 1.552 | 1.44, 1.68, 1.52 | linear |
| glob_alternatives | 2.190 | 4.071 | 9.736 | 21.110 | 1.86, 2.39, 2.17 | linear |
| nested_quotes | 0.683 | 1.154 | 1.953 | 1.373 | 1.69, 1.69, 0.70 | linear |

Verdicts on each line (classifier result):
- nested_substitution: Some(UnseenCode)
- alias_chain: Some(UnseenCode)
- arithmetic: None
- glob_alternatives: None
- nested_quotes: Some(UnseenCode)
