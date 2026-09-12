#!/usr/bin/env python3
"""Offline schema correction for PG02; cannot restart or validate its panel."""
from score_pg02 import cell_gate


def compatible_cell_gate(q, result, campaign, usage, progress, max_drain):
    # CampaignReport intentionally omits None. Normalize only this documented
    # optional field, without altering the frozen scorer or native artifacts.
    normalized = dict(campaign)
    normalized.setdefault('slot_retention', None)
    return cell_gate(q, result, normalized, usage, progress, max_drain)
