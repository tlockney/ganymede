# Python in a journal (ipykernel)

A Python kernel runs in an environment someone set up, and has only
the packages installed there. Check before relying on one, and install
what's missing from the notebook so the next run knows it needs it.

## Libraries

```python
%pip install polars==1.31.0 matplotlib==3.10.3
```

Put the install in a cell of its own near the top, with exact versions,
and run it with `cell_run` so it stays in the notebook. Restart the
kernel after installing a package that was already imported. A probe
that checks for a package (`import polars`) can run with
`record: false`; an install can't, since the next run needs it too.

## Output

A cell's last expression is its result. `IPython.display` builds rich
output, as cdp-kernel's `jupyter` global does in TypeScript:

```python
from IPython.display import display, Markdown, SVG, Image

display(Markdown(f"### Weekly failure rate\n\nAverage: **{avg:.1f}**"))
display(Markdown(weekly.to_pandas().to_markdown(index=False)))  # a table you can read back
display(SVG(svg_text))
display(Image(filename="chart.png"))

handle = display(Markdown("Starting"), display_id=True)    # show now
handle.update(Markdown(f"Processed {n} of {total}"))       # update in place
```

`to_pandas` needs pandas and pyarrow, and `to_markdown` needs
tabulate. The agent sees text and Markdown; HTML is shown with its tags
stripped, so prefer Markdown tables over a data frame's HTML for numbers
you want to read back.

## Charts

matplotlib draws a PNG under the cell, which the server can send to
you and `cell_snapshot` returns:

```python
import matplotlib.pyplot as plt

fig, ax = plt.subplots(figsize=(8, 4))
ax.bar(weeks, rates)
ax.set_ylabel("failed runs (%)")
fig.tight_layout()
```

Altair and Plotly draw in the frontend with JavaScript: a person sees
the chart, the agent sees only text, and a published page shows it only
if the output also holds an image. For a chart you need to read, use
matplotlib.

## Widgets

ipywidgets gives ready-made controls; `interact` wires one to a
function and redraws its output on each change:

```python
%pip install ipywidgets==8.1.7
```

```python
from ipywidgets import interact, IntSlider

@interact(min_runs=IntSlider(value=0, min=0, max=200, step=10))
def plot_from(min_runs):
    rows = weekly.filter(pl.col("runs") >= min_runs)
    fig, ax = plt.subplots(figsize=(8, 4))
    ax.bar(rows["week"], rows["failed"])
    plt.show()
```

The agent sees only `text/plain` for a widget, and a published page
leaves it out, so follow the widget with a static chart for the value
the finding names.
