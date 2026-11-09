| pattern | 1000 median ms | 2000 median ms | 4000 median ms | 8000 median ms | doubling ratios | verdict |
|---|---|---|---|---|---|---|
| nested_substitution | 67.516 | 88.819 | 153.283 | 384.408 | 1.32, 1.73, 2.51 | unclear (ratio between 2.5 and 3.0) |
| alias_chain | 115.657 | 196.152 | 262.866 | 456.335 | 1.70, 1.34, 1.74 | linear |
| arithmetic | 33.952 | 38.378 | 47.256 | 52.940 | 1.13, 1.23, 1.12 | linear |
| glob_alternatives | 57.656 | 76.088 | 119.072 | 251.884 | 1.32, 1.56, 2.12 | linear |
| nested_quotes | 30.543 | 40.622 | 37.735 | 53.194 | 1.33, 0.93, 1.41 | linear |

Verdicts on each line (classifier result):
- nested_substitution: Some(UnseenCode)
- alias_chain: Some(UnseenCode)
- arithmetic: None
- glob_alternatives: None, Some(UnseenCode)
- nested_quotes: Some(UnseenCode)
