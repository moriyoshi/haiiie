# What residual coding costs

haiiie returns the exact top-k under the stored residual-code score. The
difference between that order and exhaustive cosine over the source embeddings
is quantization error. It is the only approximation in residual search, and it
must be measured on your corpus before the model is deployed.

## A recall figure needs its frame

Recall@k is a property of the encoder, corpus, query set and rank boundary. A
corpus whose kth and (k+1)th similarities are nearly tied is harder than one
with a wide gap, even under the same model. Report at least the exhaustive
similarities at ranks 1, k, k+1 and 10k beside recall.

Keep three document roles separate:

1. Fit documents train the residual atoms, ridge decoder and norm interval.
2. Validation documents and queries select among seeds or other declared model
   choices.
3. Test documents and queries measure the selected construction once.

The offline fitter consumes only the fit rows. For each fixed seed, encode the
validation rows, run exhaustive exact integer top-k over their 64-byte codes,
and compare those ids with exhaustive float64 cosine truth. Select on validation
and quote test only after that choice is frozen.

## The measured 64-byte model

The selected layout stores 500 residual signs and a 12-bit reciprocal-norm code
per document. Its shared decoder uses signed 12-bit coefficients and its query
uses signed 16-bit components. Serving ranks the resulting integer expression
exactly; the figures below measure only the difference between that stored-code
order and float cosine.

The confirmation corpus contains 512-component COCO image embeddings. Each of
three predeclared seeds fit 16,384 documents. The new split held 44,355
validation and 44,356 test documents, with 512 disjoint queries for each and no
evaluation query reused from codec selection. Float64 normalized cosine over
the complete corresponding document set is truth.

| Construction | validation recall@10 | test recall@10 |
|---|---:|---:|
| 500 signs + 12-bit norm, three-seed mean | 0.7681 | **0.7631** |
| 512 signs + stored 64-bit norm, 72-byte control | 0.7732 | **0.7671** |

The fixed 64-byte model gives up 0.40 percentage points of test recall to the
72-byte control in this frame. That is a storage trade, not parity.

The supported native fitter uses its persisted SplitMix64 plus Box-Muller
generator so model construction does not depend on a Python random-generator
version. On the same three seeds it averaged 0.7669 validation and 0.7611 test
recall. The complete native seed-43 command produced 0.7711 validation and
0.7623 test recall and byte-identical model files with one and four workers.
These numbers validate the implementation on this corpus; they do not predict
yours.

## No retained-float fallback

Residual ingest consumes a document embedding, emits its 64-byte code and
discards the float input. Query embeddings are transient. The server has no
float side-store and no second-stage rerank mode, so storage stays at the fixed
code size and every returned order has the exact stored-code meaning described
by the wire contract.

An earlier SimHash plus retained-float rerank experiment reached high recall by
keeping the original document vectors. That violates the product's storage
constraint and has been removed from the library, service and protocol. Its
measurements remain development history rather than a supported configuration.

## What was tried and rejected

A pure norm-free 64-byte OPQ control reached 0.7254 test recall@10, below the
500-sign plus norm layout. A categorical bitmap construction reached 0.7043 and
also scored more slowly through persisted bitmaps than a packed-row scan.
Keeping a small norm field in each packed row survived both accuracy and
execution controls.

Bounding a block by distance from its code centroid eliminated 0.0% of blocks
at every tested granularity from 4,096 documents down to 16 under arbitrary and
cluster-ordered ids. The radius term itself exceeded the useful threshold. That
path was deleted rather than left as an unmeasured tuning option.
