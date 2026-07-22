---
title: "VISION — GRN Benchmarking & Single-Cell Omics Research"
date: 2026-07-15
status: living
tags: [single-cell, grn, gene-regulatory-networks, regvelo, genernib, perturbation, spatial-omics]
---

# VISION — GRN Benchmarking & Single-Cell Omics Research

> **Plain-language overview of what we're doing, why, and where it connects to the bigger spatial-omics vision.**

---

## 1. The Big Picture

We're studying **Gene Regulatory Networks (GRNs)** — the wiring diagrams that control which genes turn on/off in a cell, and how that wiring produces cell identity and fate transitions.

The concrete project right now: **contributing RegVelo to the geneRNIB benchmark** — a living benchmark for GRN inference methods. geneRNIB compares 15 methods across 13 datasets using perturbation-based ground truth. We wrapped RegVelo (a GRN-informed RNA velocity method from the Theis lab) as a new method and are benchmarking it against the field.

The longer arc: this is the entry point into the broader **spatial single-cell omics** vision — the "computer as laboratory" idea from AGENTS.md. GRNs are the regulatory program; spatial context determines which programs are active where. The geneRNIB work teaches us the methods landscape and gives us a credible benchmarking foothold. The spatial GRN frontier (STARNet and beyond) is where the real green field is.

---

## 2. What Is a GRN?

A **Gene Regulatory Network** is a directed graph where:
- **Nodes** are genes (typically transcription factors — TFs — and their target genes)
- **Edges** mean "TF X regulates gene Y" (activation or repression)

Think of it as the cell's **software**: DNA is the hard drive (the code), but the GRN is the running program — which instructions are executing, in what order, producing what output. The same genome produces a neuron, a T cell, or a tumor cell not by changing the DNA, but by running different regulatory programs. The GRN *is* that program.

**Why it matters:** if you know the GRN, you can:
- **Predict** what happens when you perturb a gene (drug target, knockout)
- **Understand** how a stem cell decides to become a specialized cell type
- **Identify** master regulators — the TFs whose perturbation cascades through the network
- **Model** disease as a wiring problem, not just a mutation problem

**Why it's hard:** you can't observe the GRN directly. You measure RNA expression (a snapshot of which genes are on), then *infer* the regulatory relationships. But co-expression ≠ regulation. The core challenges:

1. **Co-expression ≠ regulation** — correlation doesn't prove causation
2. **Directionality** — correlation is undirected; you need DNA motif evidence to know TF → target
3. **Context-specificity** — the same TF regulates different targets in different cell types
4. **Causality** — only perturbation data (knock out the TF, measure what changes) gives ground truth

Every GRN inference method is a different bet on how to handle these challenges with limited data.

---

## 3. What Is geneRNIB?

**geneRNIB** = **gene** **R**egulatory **N**etwork **I**nference **B**enchmark.

It's a **living benchmark** (maintained by the openproblems-bio community) that systematically evaluates GRN inference methods on equal footing. Think ImageNet for gene regulatory networks — but instead of labeling cats vs. dogs, it asks: "given single-cell expression data, can your method recover the true regulatory wiring?"

### What makes it different from prior benchmarks

Previous benchmarks (BEELINE, 2020) evaluated methods against synthetic data and bulk ChIP-seq references. geneRNIB uses **perturbation-based ground truth** — the gold standard. When you knock out a gene and measure which other genes change, those changed genes are (mostly) the real targets. Causal evidence, not just correlation.

### The structure

- **15 methods** benchmarked: positive_control, pearson_corr (baselines), GRNBoost2, PPCOR, Portia, SCENIC, SCENIC+, CellOracle, FigR, GRaNIE, scGLUE, scPRINT, scGPT, Geneformer, negative_control
- **13 datasets** spanning different cell types, perturbation types, and modalities
- **Multiple metrics** per dataset (not all metrics apply to all datasets — see §6)
- **Viash pipeline** — each method is a containerized component runnable reproducibly

### The method families

| Family | Methods | Wager |
|--------|---------|-------|
| Co-expression + motif | SCENIC, GRNBoost2, Portia | Correlation captures regulation; motif filters false positives |
| Multi-omics / ATAC-integrated | SCENIC+, FigR, GRaNIE, scGLUE | Accessibility evidence converts co-expression into regulation |
| Within-cluster regression | CellOracle | Per-cluster predictive models capture context-specific regulation |
| Foundation models | scGPT, scPRINT, Geneformer | Pretrained transformers capture regulatory structure |
| Correlation baselines | pearson_corr, ppcor | Simple correlations — the floor to beat |
| Velocity-informed | **RegVelo (ours)** | GRN-structured velocity — regulatory causality shapes dynamics |

### Why we're contributing

1. **RegVelo is a genuinely new family** — velocity-informed GRN inference. Nobody else in the benchmark does this.
2. **Benchmarking is the honest contribution** — the field's biggest unmet need isn't new methods, it's honest validation against perturbation ground truth.
3. **It's the on-ramp** — learning the methods landscape, datasets, metrics, and pipeline infrastructure positions us for the bigger spatial GRN work.

---

## 4. Why RegVelo?

**RegVelo** (Wang et al., 2024 bioRxiv → 2026 Cell) is from the Theis lab (the same group behind scanpy, scVelo, CellRank — the scverse ecosystem).

### What it does

Standard RNA velocity (scVelo) infers cell-state direction from the ratio of spliced to unspliced mRNA — but it's kinematically naive. It traces RNA kinetics without any account of *why* the cell is moving. No regulatory logic.

RegVelo makes velocity **gene-regulatory-informed**: genes are coupled through an explicit GRN, and transcription rates respect regulatory causality. The velocity field doesn't just follow RNA kinetics — it follows the regulatory program's constraints. The trajectory and the regulatory wiring become one object.

### The technical idea

RegVelo is a **variational autoencoder (VAE)** where:
- The encoder maps single-cell expression → latent GRN
- The GRN structure acts as a **prior** on the generative model — the network constrains which transitions are possible
- Decoded RNA velocity respects the regulatory graph, not just splicing kinetics

### Why we picked it

1. **It's the trajectory ↔ GRN synthesis** — the most exciting open frontier identified in our deep-dive. RegVelo is the first real stab at making velocity regulatory-informed.
2. **It's from the Theis lab** — robust infrastructure, clean code, integrates with the scverse ecosystem.
3. **Nobody in geneRNIB does this** — fills a genuine gap in the benchmark's method coverage.
4. **The degenerate-velocity caveat is an interesting story** (see §7) — even without real splicing signal, the GRN architecture alone might help.

### Repo

[`theislab/regvelo`](https://github.com/theislab/regvelo) — PyPI: `regvelo`.

---

## 5. The Datasets — What Each One Is

Here's what every dataset in geneRNIB actually is, from the config and primary literature.

### Quick-reference table

| Dataset | Cell type | Perturbation | Data | Modality | Time | Raw counts? | Source |
|---------|-----------|-------------|------|----------|------|-------------|--------|
| **op** | PBMC | Drugs | sc | Multiomics | 24 hrs | Yes | Open Problems (NeurIPS 2021) |
| **parsebioscience** | PBMC | Cytokines | sc/bulk | Transcriptomics | 24 hrs | No | Parse Bioscience |
| **300BCG** | PBMC | Chemicals (LPS) | sc | Transcriptomics | T0 + 3 months | Yes | 300BCG BCG vaccine cohort |
| **replogle** | K562 | Knockout (CRISPRi) | sc/bulk | Transcriptomics | 7 days | Yes | Replogle et al. 2022, Cell |
| **norman** | K562 | Activation (CRISPRa) | sc | Transcriptomics | 7 days | Yes | Norman et al. 2019, Science |
| **adamson** | K562 | Knockout | sc | Transcriptomics | 7 days | Yes | Adamson et al. 2016 |
| **xaira_HEK293T** | HEK293T | Knockout | sc/bulk | Transcriptomics | 7 days | Yes | Xaira Therapeutics |
| **xaira_HCT116** | HCT116 | Knockout | sc/bulk | Transcriptomics | 7 days | Yes | Xaira Therapeutics |
| **nakatake** | SEES3 (PSC) | Overexpression | bulk | Transcriptomics | 2 days | No | CellOracle (Kamimoto 2023) |
| **MSCIC** | BMMC | Observational | sc | Multiomics | N/A | Yes | NeurIPS 2021 (GSE194122) |
| **soundlife** | CD4T | Observational | sc | Transcriptomics | Year 1→2 | Yes | SoundLife flu vaccine cohort |
| **soundlife_vaccine** | B | Vaccination | sc | Transcriptomics | Y1D0+7→Y1+2D90 | Yes | SoundLife flu vaccine cohort |

### Plain-language explanations

**op — "Open Problems":** The OPSCA dataset from the NeurIPS 2021 competition. **PBMC** (peripheral blood mononuclear cells — immune cells) perturbed with **drugs**. Multiomics (RNA + ATAC from same cells), 24-hour measurement. The standard reference perturbation dataset in the community.

**parsebioscience:** PBMC perturbed with **cytokines** (signaling molecules immune cells use to communicate). 24 hours. From Parse Bioscience (commercial single-cell platform). No raw counts — preprocessed/log-normalized.

**300BCG:** PBMC from the 300BCG cohort — BCG (tuberculosis vaccine) effects on immune training. Perturbation is chemical (LPS — bacterial endotoxin). Measurements at baseline and 3 months post-vaccination. Captures **trained immunity** — how the immune system "remembers" non-specific stimulation. Single perturbation condition (LPS only).

**replogle:** K562 (chronic myeloid leukemia cell line) with **genome-scale CRISPRi knockouts**. The landmark Replogle et al. 2022 paper — knocked out ~11,000 genes individually, measured transcriptomic effects in single cells. Largest systematic perturbation dataset in existence. 7-day timepoint.

**norman:** K562 with **CRISPRa activation** (turns genes ON rather than disabling them). Norman et al. 2019, the foundational Perturb-seq paper. Includes combinatorial (two-gene) perturbations that reveal genetic interactions. 7 days.

**adamson:** K562 with **knockout** perturbations. Adamson et al. 2016, an earlier Perturb-seq paper. *Not in the plan's original list of 11 datasets but was run opportunistically during the pilot.*

**xaira_HEK293T & xaira_HCT116:** Two knockout datasets from **Xaira Therapeutics** (AI-focused drug discovery company). HEK293T = Human Embryonic Kidney (workhorse molecular biology line). HCT116 = colorectal carcinoma (cancer line). Both test whether methods generalize across cell types.

**nakatake:** SEES3 — a **pluripotent stem cell (PSC)** line. Perturbation is **overexpression**. From the CellOracle paper (Kamimoto et al. 2023). **Bulk RNA-seq** (not single-cell — the only bulk-only dataset). 2-day timepoint. No raw counts. This is the dataset RegVelo scored near-zero on in the pilot.

**MSCIC:** BMMC (Bone Marrow Mononuclear Cells) — **observational** (no perturbation). Multiomics (10x Multiome: snRNA + snATAC). From NeurIPS 2021 Open Problems (GSE194122). 10 donors, ~29K cells. Ground truth comes from cell-type identity, not measured knockouts.

**soundlife:** CD4T cells (T helper cells) — **observational**. From the SoundLife flu-vaccination longitudinal cohort. 10 donors, Year 1 Day 0 → Year 2 Day 0. Captures natural immune variation over time. ~25K cells.

**soundlife_vaccine:** B cells from the same SoundLife cohort, tracking the **vaccination response**. Year 1 Day 0+7 → Year 1+2 Day 90. ~30K cells. Closest thing to a longitudinal clinical dataset in the benchmark.

### The three categories

1. **Perturbation (knockout/activation/overexpression)** — replogle, norman, adamson, xaira, nakatake. Ground truth: measured regulatory effects. **Causal gold standard.**
2. **Perturbation (drugs/cytokines/chemicals)** — op, parsebioscience, 300BCG. Ground truth: drug/cytokine response signatures. **Tests whether the GRN explains drug response.**
3. **Observational** — MSCIC, soundlife, soundlife_vaccine. No perturbation. Ground truth: cell-type identity and regulatory structure. **Tests whether the GRN captures natural variation.**

---

## 6. The Metrics — What They Measure

geneRNIB doesn't use a single score. Each dataset gets a different set of metrics, because not all ground-truth signals are available for every dataset.

### Regression (`regression`)
The core metric. After inferring a GRN, you perturb a TF *in silico* and predict the expression shift. Regression measures how well the predicted post-perturbation expression matches the measured post-perturbation expression.
- **r_precision / r_recall** — precision/recall of predicted differentially expressed genes vs. measured ones (R²-based, meaningful if > 0.001)
- **r_raw** — raw R² between predicted and measured expression vectors

This is the "does your GRN predict what happens when you perturb a gene" metric. The most important one.

### Wasserstein distance (`ws_distance`)
Distributional distance between predicted and measured post-perturbation expression distributions. More stringent than regression — asks whether the *full shape* of the response is captured. Meaningful if > 0.5.

### Virtual Cell (`vc`)
Simulates a "virtual cell" — predicts the expression profile of a cell after perturbation. R² between predicted and actual. Meaningful if > 0.01. **The "can you simulate a perturbed cell" metric.**

### SEM (Structural Equation Modeling)
Fits a structural equation model to the GRN and tests whether the inferred regulatory structure is consistent with the measured covariance structure. Meaningful if > 0.01. **Tests whether the GRN topology is statistically defensible.**

*Note: SEM crashed on adamson during the pilot — `ValueError: cannot specify integer bins when input data contains infinity`. Needs debugging.*

### TF Recovery (`tf_recovery`)
Can you **recover the perturbed TF** from the expression signature alone? Uses t-statistics (meaningful if > 2.0, i.e., p < 0.05). **Tests whether the GRN correctly identifies the causal regulator.**

### TF Binding (`tf_binding`)
Compares inferred TF → target edges against **known TF binding evidence** (ChIP-seq, motif databases). F1 score (meaningful if > 0.05). **Tests whether the edges are biologically real.**

### Gene Set Recovery (`gs_recovery`)
Tests whether the GRN's predicted targets match **known gene sets / pathways** associated with that TF. F1 score (meaningful if > 0.05). **Tests biological plausibility at the pathway level.**

### Replicate Consistency (`replicate_consistency`)
For datasets with biological replicates, measures whether the GRN's predictions are **consistent across replicate experiments**. Wildly different networks for replicate samples = red flag. Meaningful if > 0.3.

### Which datasets use which metrics

| Dataset | Metrics |
|---------|---------|
| replogle | regression, ws_distance, tf_recovery, tf_binding, sem, gs_recovery, vc |
| norman | regression, ws_distance, tf_binding, gs_recovery, vc, tf_recovery |
| adamson | regression, sem, gs_recovery |
| nakatake | regression, gs_recovery, vc |
| op | regression, vc, replicate_consistency, sem, gs_recovery |
| 300BCG | regression, vc, replicate_consistency, tf_binding, gs_recovery |
| parsebioscience | regression, vc, replicate_consistency, tf_binding, sem, gs_recovery |
| xaira_HEK293T | regression, ws_distance, tf_recovery, tf_binding, sem, gs_recovery, vc |
| xaira_HCT116 | regression, ws_distance, tf_recovery, tf_binding, sem, gs_recovery, vc |
| MSCIC | regression, gs_recovery, replicate_consistency |
| soundlife | regression, replicate_consistency, tf_binding, gs_recovery |
| soundlife_vaccine | regression, replicate_consistency, tf_binding, gs_recovery |

---

## 7. The Scientific Question

### The hypothesis

**Does incorporating a GRN as a structural prior in a velocity model (RegVelo) improve GRN recovery over methods that infer networks from expression alone?**

This tests the trajectory ↔ GRN synthesis: whether making the dynamical model regulatory-informed helps you recover the *static* regulatory network better than methods that never consider dynamics.

### The degenerate-velocity caveat

RegVelo's power comes from RNA velocity — the spliced/unspliced ratio that tells you which direction a cell is moving. But in many benchmark datasets, the velocity signal is **degenerate**: single-timepoint snapshots where splicing dynamics haven't produced meaningful velocity vectors.

So what RegVelo is testing in the pilot is the **architecture alone** — does the GRN-structured VAE, even without real velocity signal, produce better regulatory networks than correlation baselines?

### Early pilot signal

| Dataset | Metric | RegVelo score | Read |
|----------|--------|---------------|------|
| **op** | r2_raw | 0.217 | **Respectable** — non-degenerate; architecture is doing something |
| **op** | r_precision | 0.219 | Real signal — beats the null |
| **nakatake** | r2_raw | 0.006 | **Near-zero** — architecture doesn't help here |
| **nakatake** | vc | 0.0002 | Essentially zero |
| **adamson** | — | **CRASHED** | SEM metric failed: infinity values in data |

**Interpretation:** op (PBMC, drugs, multiomics) shows RegVelo's architecture produces non-trivial predictions even without strong velocity. nakatake (stem cells, overexpression, bulk-only) shows it doesn't help universally. Too little data to conclude, but op suggests it's **not a universal failure** — worth continuing.

---

## 8. Where We Are Now

### Phase 1 — Method wrapper: ✅ COMPLETE

The RegVelo method is wrapped for geneRNIB:
- `script.py` — the inference script (reads RNA + TF list, outputs GRN as h5ad)
- `config.vsh.yaml` — the viash component config
- `README.md` — documentation

Located at: `reference/task_grn_inference/src/methods/regvelo/`

### Phase 2 — Benchmark run: ⚠️ PARTIALLY STARTED

A pilot run was done on **3 datasets** using the test subset data (380 MB, not the full 24 GB benchmark set):

| Dataset | Prediction | Eval | Result |
|----------|------------|------|--------|
| **op** | ✅ | ✅ EXIT=0 | r2_raw=0.217, r_precision=0.219 |
| **nakatake** | ✅ | ✅ EXIT=0 | r2_raw=0.006, vc=0.0002 |
| **adamson** | ✅ | ❌ EXIT=1 | SEM crashed: infinity values |

A `pearson_corr` baseline was also run alongside RegVelo.

### What's missing for a real Phase 2

1. **9 of 12 datasets not run** (only op, nakatake, adamson attempted)
2. **adamson eval failure un-debugged** — SEM chokes on infinity values; needs a NaN/inf filter
3. **No full benchmark data** — only the 380 MB test subset; the 24 GB full set isn't downloaded
4. **No singularity images** (48 GB) — needed for the full viash pipeline
5. **viash not installed**
6. **Consensus prior not updated** for RegVelo
7. **No comparison vs all 15 methods** — only pearson_corr baseline so far
8. **No analysis writeup**

### Phase 3 — Write-up + contribution: not started

---

## 9. The Longer-Term Vision: Spatial GRNs

The geneRNIB work is the foothold. The real target is **spatially-resolved GRN inference** — regulatory networks that vary across tissue geography.

### Why spatial

Regulation is local. A cell's regulatory program depends on its neighbors, its position in tissue, and the signaling molecules it receives from surrounding cells. A GRN that ignores spatial context is a GRN that ignores half the problem.

### STARNet (2025)

The near-sole entrant in spatial GRN inference. Integrates spatial transcriptomics + ATAC to infer GRNs with spatial context. Brand new, green field. From the deep-dive: "nearly the only tool doing spatially-resolved GRN inference; green field."

### The arc

```
geneRNIB benchmarking (now)
    → learn methods, datasets, metrics, pipeline infrastructure
    → RegVelo contribution + publication
    → spatial GRN methods (STARNet and beyond)
    → "computer as laboratory" — simulate, optimize, make useful
```

This connects directly to AGENTS.md's vision: a PapersWithCode-style platform for spatial single-cell omics, deeply indexed, with an autonomous agent that trails GitHub for new work, alerts on papers, models the research landscape, and mines cross-study patterns.

---

## 10. Agent Architecture

### Current setup

- **crow-cli** — the agent (ACP-native, minimal genome digital organism)
  - GitHub: crow-cli/crow-cli
  - Two-layer: crow-cli (thinking) + crow-mcp (tools)
- **Sidex** (forked as odellus/sidex) — VS Code rebuilt on Tauri, used as the Crow IDE
  - Three crow-cli agents side by side in purple panes

### Agent roles in this project

| Agent | Role |
|-------|------|
| Orchestrator | Plans, delegates, reviews, writes docs (VISION, PLAN) |
| Worker (rampant-skua-of-excellent-fragrance) | Runs benchmarks, writes code, debugs, reads repos |
| (possible future) Specialized GRN agent | Lives in the methods/papers, maintains knowledge base |

### The question of specialization

Thomas raised whether to build specialized agents or just use crow-cli directly. Current thinking: crow-cli is general-purpose enough. The specialization lives in the *documents* (deep-dives, VISION, AGENTS.md) — the agent reads those to get up to speed. No need for a separate model or fine-tuned agent yet. If the workflow becomes repetitive enough to warrant it, a specialized agent can be carved out later.

---

## 11. References

### Core papers

| Paper | Citation | What it is |
|-------|----------|------------|
| **geneRNIB** | Nourisa et al., bioRxiv 2025. doi:10.1101/2025.02.25.640181 | The living GRN inference benchmark we're contributing to |
| **RegVelo** | Wang et al., bioRxiv 2024 (doi:10.1101/2024.12.11.627935) → Cell 2026 (S0092-8674(26)00457-5) | GRN-informed RNA velocity — the method we're benchmarking |
| **Norman** | Norman et al., 2019, Science 365(6455):aax4438 | Foundational Perturb-seq; CRISPRa activation in K562; genetic interaction manifolds |
| **Replogle** | Replogle et al., 2022, Cell 185(14):2559-2575 | Genome-scale Perturb-seq; CRISPRi knockout in K562; ~11K genes |
| **CellOracle** | Kamimoto, Hoffman, Morris et al., 2023, Nature Communications | Dissecting cell identity via network inference and in silico gene perturbation; nakatake dataset source |
| **Open Problems** | Lance et al., NeurIPS 2021 (GSE194122) | Multimodal single-cell integration; op + MSCIC dataset source |
| **STARNet** | Hu et al., 2025, Cell Research 35(11):859-875 | Spatially-resolved GRN inference (spatial transcriptomics + ATAC) |
| **BEELINE** | Pratapa et al., 2020, Nature Methods 17:147-154 | Prior GRN benchmarking framework (synthetic + bulk ChIP-seq references) |

### Repos

| Repo | What |
|------|------|
| [theislab/regvelo](https://github.com/theislab/regvelo) | RegVelo — the method we're contributing |
| [aertslab/pySCENIC](https://github.com/aertslab/pySCENIC) | SCENIC — the workhorse GRN method |
| [aertslab/scenicplus](https://github.com/aertslab/scenicplus) | SCENIC+ — enhancer-driven GRNs from multiome |
| [morris-lab/CellOracle](https://github.com/morris-lab/CellOracle) | CellOracle — GRN + in-silico perturbation |
| [theislab/scvelo](https://github.com/theislab/scvelo) | scVelo — standard RNA velocity |
| [scverse/scanpy](https://github.com/scverse/scanpy) | scanpy — the Python single-cell toolkit |

### Companion documents

- `deep-dives/grn-inference-single-cell.md` — the full methods landscape, repos, and contribution vectors
- `deep-dives/sota-computational-sysbio-single-cell-omics.md` — broader computational systems biology context
- `AGENTS.md` — Thomas's background, the stack, the "computer as laboratory" vision
- `reference/task_grn_inference/src/utils/config.py` — the benchmark config (datasets, methods, metrics)

---

*Last updated: July 15, 2026. Living document — update as the benchmark progresses.*

This rules.