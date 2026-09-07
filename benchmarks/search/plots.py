#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Render publication figures from complete, compatible evaluation matrices.

The runner itself has no plotting dependency. Install requirements-plots.txt
only on the machine preparing publication artifacts.
"""
from __future__ import annotations

import argparse
import html
import json
from pathlib import Path
import statistics

import eval as evaluation


def records(path):
    if not path.exists():
        return []
    with path.open() as stream:
        return [json.loads(line) for line in stream if line.strip()]


def completions(items, cost):
    """Empirical verified fraction; censoring is never imputed as a victory."""
    events = sorted(item['result'][cost] for item in items
                    if (item.get('result') or {}).get('solved') and item['result'].get(cost) is not None)
    return [0] + events, [0] + [(index + 1) / len(items) for index in range(len(events))]


def numeric_metrics(progress):
    keys = set()
    for point in progress:
        for group in ('progress', 'milestones'):
            for key, value in (point.get(group) or {}).items():
                if isinstance(value, (int, float)):
                    keys.add((group, key))
    return sorted(keys)


def render(runs, out):
    if out.exists():
        raise ValueError('figure output directory must be new')
    if len({label for label, _ in runs}) != len(runs):
        raise ValueError('run labels must be unique')
    # Prevent an attractive but invalid historical comparison. Different
    # workloads/budgets belong in separate figures, never in an averaged line.
    for _, matrix in runs[1:]:
        evaluation.compare(runs[0][1], matrix)
    panels = [(label, path, evaluation.load_results(path)) for label, path in runs]
    groups = sorted({(x['case'], x['search_request'].get('workers'),
                      x['search_request'].get('memory_mib')) for x in panels[0][2]})
    import matplotlib
    matplotlib.use('Agg')
    import matplotlib.pyplot as plt
    from matplotlib.ticker import MaxNLocator
    plt.rcParams.update({'font.size': 10, 'axes.spines.top': False, 'axes.spines.right': False,
                         'axes.grid': True, 'grid.alpha': .18, 'svg.fonttype': 'none'})
    colors = plt.get_cmap('tab10').colors
    out.mkdir(parents=True)
    figures = []
    def save(fig, name, caption):
        fig.savefig(out / (name + '.svg'), bbox_inches='tight')
        fig.savefig(out / (name + '.png'), dpi=150, bbox_inches='tight')
        plt.close(fig)
        figures.append((name, caption))

    fig, axes = plt.subplots(1, 4, figsize=(19, max(4, len(groups) * .45)), sharey=True)
    labels = [f'{case} · {workers} workers · {memory} MiB' for case, workers, memory in groups]
    for run_index, (label, _, items) in enumerate(panels):
        color = colors[run_index % len(colors)]
        summaries = {(r['case'], r['workers'], r['memory_mib']): r for r in evaluation.aggregates(items)}
        for index, group in enumerate(groups):
            row = summaries[group]
            y = index + (run_index - (len(panels) - 1) / 2) * .16
            fraction, interval = row['solve_fraction_of_valid'], row['solve_fraction_wilson95']
            if fraction is not None:
                axes[0].errorbar(fraction, y, xerr=[[fraction - interval[0]], [interval[1] - fraction]],
                                 fmt='o', color=color, capsize=2, label=label if index == 0 else None)
            subset = [x for x in items if (x['case'], x['search_request'].get('workers'),
                       x['search_request'].get('memory_mib')) == group]
            values = [[(x.get('result') or {}).get('frames_per_second') for x in subset],
                      [x.get('max_process_rss_bytes') for x in subset],
                      [x.get('peak_disk_logical_bytes_sampled') for x in subset]]
            for axis, values, scale in zip(axes[1:], values, (1000, 1024**2, 1024**2)):
                available = [v / scale for v in values if v is not None]
                if available:
                    axis.scatter(available, [y] * len(available), color=color, s=15, alpha=.4)
                    axis.scatter(statistics.median(available), y, color=color, marker='|', s=120)
    axes[0].set_yticks(range(len(groups)), labels)
    axes[0].invert_yaxis()
    axes[0].set_xlim(-.05, 1.05)
    for axis, title in zip(axes, ('Verified solve fraction (Wilson 95%)', 'Search emulator kframes/s',
                                 'OS peak process RSS (MiB)', 'Sampled peak disk (MiB)')):
        axis.set_title(title)
        axis.xaxis.set_major_locator(MaxNLocator(4))
    axes[0].legend(loc='upper center', bbox_to_anchor=(.5, -.05))
    fig.suptitle('Fixed workload and resource panels · points are individual seeds', y=1.02)
    fig.tight_layout()
    save(fig, 'panels', 'Solve fractions use valid runs; infrastructure errors remain in the underlying results. '
         'Resource dots show every available seed measurement; bars show medians. RSS and logical memory are distinct.')

    for case, workers, memory in groups:
        name = f'{case}-w{workers}-m{memory}'
        fig, axes = plt.subplots(2, 3, figsize=(16, 8))
        all_progress = []
        progress_series = []
        for run_index, (label, path, items) in enumerate(panels):
            color = colors[run_index % len(colors)]
            subset = [x for x in items if (x['case'], x['search_request'].get('workers'),
                       x['search_request'].get('memory_mib')) == (case, workers, memory)]
            x, y = completions(subset, 'frames_to_first_victory')
            extent = max(((x.get('result') or {}).get('frames_emulated', 0) for x in subset), default=0)
            axes[0, 0].step(x + [max(extent, x[-1])], y + [y[-1]], where='post', color=color, label=label)
            for seed_index, item in enumerate(subset):
                progress = records(path / item['cell'] / 'campaign/progress.jsonl')
                resources = records(path / item['cell'] / 'resources.jsonl')
                progress_series.append((label, item['search_request'].get('seed'), color, progress))
                all_progress.extend(progress)
                timed = [p for p in progress if p.get('search_elapsed_millis') is not None]
                seconds = [p['search_elapsed_millis'] / 1000 for p in timed]
                axes[0, 1].plot(seconds, [p['frames_emulated'] for p in timed], color=color, alpha=.5)
                axes[0, 2].plot(seconds, [p['resident_memory_bytes'] / 1024**2 for p in timed], color=color, alpha=.5)
                rss = [p for p in resources if p.get('rss_bytes') is not None]
                axes[1, 0].plot([p['elapsed_seconds'] for p in rss], [p['rss_bytes'] / 1024**2 for p in rss], color=color, alpha=.5)
                axes[1, 1].plot([p['elapsed_seconds'] for p in resources],
                                [p['disk']['logical_bytes'] / 1024**2 for p in resources], color=color, alpha=.5)
                axes[1, 2].plot([p['frames_emulated'] for p in timed],
                                [p['historical_cells'] for p in timed], color=color, alpha=.5)
        titles = [('Verified fraction by admitted frames', 'Emulator frames', 'Fraction of registered trials'),
                  ('Emulator work over search time', 'Search seconds', 'Admitted frames'),
                  ('Logical search memory', 'Search seconds', 'MiB charged'),
                  ('Process group RSS · all phases', 'Elapsed seconds', 'MiB sampled'),
                  ('Disk footprint · all phases', 'Elapsed seconds', 'MiB logical, sampled'),
                  ('Observed selection-cell coverage', 'Emulator frames', 'Distinct historical cells')]
        for axis, (title, xlabel, ylabel) in zip(axes.flat, titles):
            axis.set(title=title, xlabel=xlabel, ylabel=ylabel)
            axis.xaxis.set_major_locator(MaxNLocator(5))
        axes[0, 0].set_ylim(0, 1.05)
        axes[0, 0].legend()
        fig.suptitle(f'{case} · {workers} workers · {memory} MiB · one trace per seed')
        fig.tight_layout()
        save(fig, name, 'Unsolved and failed trials never become completion events. Lines stop at observed work; '
             'there is no extrapolation beyond the run budget. Resource sampling includes preparation and verification. '
             'Coverage depends on the frozen archive identity and can combine different explored branches.')
        metrics = numeric_metrics(all_progress)
        if metrics:
            columns = min(3, len(metrics))
            rows = (len(metrics) + columns - 1) // columns
            fig, axes = plt.subplots(rows, columns, figsize=(columns * 5, rows * 3), squeeze=False)
            for axis, (group, field) in zip(axes.flat, metrics):
                for label, seed, color, progress in progress_series:
                    points = [p for p in progress if isinstance((p.get(group) or {}).get(field), (int, float))]
                    axis.step([p['frames_emulated'] for p in points], [p[group][field] for p in points],
                              where='post', alpha=.5, color=color)
                axis.set(title=f'{group}.{field}', xlabel='Emulator frames')
                axis.xaxis.set_major_locator(MaxNLocator(4))
            for axis in list(axes.flat)[len(metrics):]:
                axis.set_visible(False)
            fig.suptitle(f'{case} · reported observations, not an inferred scalar score')
            fig.tight_layout()
            save(fig, name + '-observations', 'These are the workload’s named report fields. Map labels and coordinates '
                 'are observations, not evidence of distance to a solution. Aggregate milestones may combine branches.')

    provenance = [{'label': label, 'results_sha256': evaluation.digest(path / 'results.json'),
                   'matrix_sha256': evaluation.digest(path / 'matrix.json')} for label, path in runs]
    evaluation.write_json(out / 'inputs.json', {'runs': provenance, 'matplotlib': matplotlib.__version__})
    content = ''.join(f'<section><a href="{name}.svg"><img src="{name}.png" alt="{html.escape(name)}"></a>'
                      f'<p>{html.escape(caption)}</p></section>' for name, caption in figures)
    (out / 'index.html').write_text('<!doctype html><html lang="en"><meta charset="utf-8">'
        '<meta name="viewport" content="width=device-width"><title>Search evaluation figures</title>'
        '<style>body{font:16px/1.5 system-ui;max-width:1500px;margin:2rem auto;padding:1rem;color:#172236}'
        'img{width:100%;height:auto}section{margin:3rem 0}p{max-width:95ch}</style>'
        '<h1>Search evaluation figures</h1><p>Series: ' + html.escape(', '.join(label for label, _ in runs)) +
        '. Each input is a complete matrix with matching workload policies and budgets. '
        'Figures remain separate from the immutable run exports. Click any chart for vector SVG.</p>' + content + '</html>')
    evaluation.write_json(out / 'checksums.json', {str(p.relative_to(out)): evaluation.digest(p)
                          for p in sorted(out.iterdir()) if p.is_file()})


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--run', action='append', required=True, metavar='LABEL=DIRECTORY')
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    try:
        runs = [(label, Path(path)) for label, path in (value.split('=', 1) for value in args.run)]
        render(runs, args.out)
    except (ValueError, KeyError, OSError) as error:
        parser.exit(2, str(error) + '\n')


if __name__ == '__main__':
    main()
