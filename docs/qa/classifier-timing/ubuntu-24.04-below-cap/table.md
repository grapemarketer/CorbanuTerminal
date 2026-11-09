| pattern | 64 median ms | 128 median ms | 256 median ms | 512 median ms | doubling ratios | verdict |
|---|---|---|---|---|---|---|
| nested_substitution | 2.186 | 4.087 | 7.959 | 15.093 | 1.87, 1.95, 1.90 | linear |
| alias_chain | 5.671 | 11.154 | 21.440 | 29.507 | 1.97, 1.92, 1.38 | linear |
| arithmetic | 0.314 | 0.483 | 0.796 | 1.204 | 1.54, 1.65, 1.51 | linear |
| glob_alternatives | 1.644 | 3.041 | 7.736 | 16.180 | 1.85, 2.54, 2.09 | unclear (ratio between 2.5 and 3.0) |
| nested_quotes | 0.521 | 0.870 | 1.469 | 0.963 | 1.67, 1.69, 0.66 | linear |

Verdicts on each line (classifier result):
- nested_substitution: Some(UnseenCode)
- alias_chain: Some(UnseenCode)
- arithmetic: None
- glob_alternatives: None
- nested_quotes: Some(UnseenCode)
