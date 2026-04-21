# nba-odds

Scrapes ESPN's NBA odds page, fetches ESPN's matchup predictor for each game, calculates Expected Value (EV) for moneyline bets, and tracks results over time.

## What it does

Each run:
1. Fetches today's NBA games and DraftKings moneylines from [espn.com/nba/odds](https://www.espn.com/nba/odds)
2. Fetches ESPN's matchup predictor win probability for each game **concurrently**
3. Calculates EV for each team's moneyline bet using the predictor probability
4. Flags **away team** bets with EV between **8.1 and 19.9** as **PICK** (away teams only)
5. Persists all games to `output/picks.db` (SQLite), grouped by day
6. Exports a human-readable `output/picks.txt` from the database each run
7. Checks the past 14 days of PICKs against ESPN's results API and updates the won/lost/profit counter

## EV Formula

```
EV = (win_probability × profit_on_$100_bet) − (lose_probability × $100)
```

Moneyline to profit conversion:
- Positive ML (e.g. +390): profit = $390
- Negative ML (e.g. -220): profit = $10,000 / 220 = $45.45

## Output

### Terminal

```
--------------------------------------------------------------------
Team                               ML      Pred%             EV
--------------------------------------------------------------------
  Monday 3/9/2026 7:00pm
Philadelphia 76ers             +390      24.2%      +18.6 PICK
Cleveland Cavaliers            -480      75.8%          -6.7
--------------------------------------------------------------------
```

### output/picks.txt

```
Total PICKs: 7 | Won: 3 | Lost: 2 | Profit: +$840.00

=== 3/9/2026 ===
[/nba/game/_/gameId/401810786/76ers-cavaliers]
Philadelphia 76ers         +390      24.2%      +18.6 PICK WON +$390.00
Cleveland Cavaliers        -480      75.8%          -6.7
```

- Only the **away team** can be flagged as a PICK — home team EV is shown but never picked
- Re-running the app will not duplicate games already in the database
- The won/lost/profit counter updates every run based on completed game results
- Only games within the past 14 days are checked for results
- Profit assumes $100 flat bets on each PICK
- On first run, any existing `picks.txt` is automatically migrated into the SQLite database

## Architecture

| Feature | Implementation |
| --- | --- |
| Concurrent HTTP fetches | `rayon` — matchup predictor pages fetched in parallel across all CPU threads |
| Persistent storage | `rusqlite` (bundled SQLite) — `games` table keyed by game URL |
| Human-readable export | `picks.txt` regenerated from the database each run |
| Unit tests | 28 tests covering all pure functions (`cargo test`) |

## Build

```bash
cargo build --release
```

Binary is at `target/release/nba-odds.exe`.

## Run

```bash
# Default: today's games from espn.com/nba/odds
cargo run

# Or run the release binary directly
./target/release/nba-odds

# Run unit tests
cargo test
```

## Run on Windows startup

1. Build the release binary (`cargo build --release`)
2. Open **Task Scheduler** → **Create Basic Task**
3. Trigger: **When I log on**
4. Action: **Start a program**
   - Program: `D:\repos\Personal\espn-odds\target\release\nba-odds.exe`
   - Start in: `D:\repos\Personal\espn-odds`

To run silently (no terminal window), create `run.vbs`:

```vbscript
Set WshShell = CreateObject("WScript.Shell")
WshShell.Run Chr(34) & "D:\repos\Personal\espn-odds\target\release\nba-odds.exe" & Chr(34), 0, False
```

Then point Task Scheduler at `wscript.exe` with argument `D:\repos\Personal\espn-odds\run.vbs`.

## Dependencies

| Crate | Purpose |
|---|---|
| `reqwest` | HTTP requests (ESPN odds page + game pages + results API) |
| `scraper` | HTML parsing |
| `clap` | CLI argument parsing |
| `serde_json` | Parsing ESPN's game results API response |
| `rayon` | Data-parallel iterators for concurrent game fetches |
| `rusqlite` | Embedded SQLite database (bundled, no system dep required) |
