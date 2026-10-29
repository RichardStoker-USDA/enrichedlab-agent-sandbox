# Preview: the season figure

`results/season_difference.pdf` and `results/season_difference.svg` are the output of
`draw_season_difference.R` in base R, drawn from `input/season_difference.csv`. That table is the long run's
comparison step's block-by-hour table as `seasonDifferenceCsv()` in `../../long_run.ts` writes it from the seeded
simulated readings; `src/season_fixture.test.ts` checks that the two are the same bytes, so the figure always shows the
table's numbers. Every value is simulated.

To regenerate, write the table and draw it with an R that has Cairo SVG support:

    node -e "import('./fixtures/long_run.ts').then((m) => process.stdout.write(m.seasonDifferenceCsv()))" > fixtures/r/season/input/season_difference.csv
    cd fixtures/r/season && pixi exec --spec "r-base>=4.5" -- Rscript draw_season_difference.R input/season_difference.csv results

The first command runs from `clients/workspace`. The 3D view and the values view read the same table from the step's
package; the R figure is the publication master.
