# Porównanie harnessów: agent vs jcode vs pi

Data pomiarów: 2026-10-19. Seria 1: 14:17–14:56. Serie `clean` i `real`: 16:18–17:40 (+0200). Autor: sesja agenta dla Artura.

## Wynik w skrócie

Wszystkie 51 przebiegów zadań w seriach `clean` i `real` przeszło 100% ukrytych testów (39/39 i 2550/2550). Każdą serię zmierzono w dwóch profilach konfiguracji:

- **clean**: każdy harness ma tylko swój login. Brak skilli, rozszerzeń, hooków i własnego system promptu (wyjątek dla pi: patrz "Profile").
- **real**: codzienna konfiguracja Artura (`~/.agents`, `~/.jcode`, `~/.pi/agent`).

| Kategoria | clean | real | Na podstawie |
|---|---|---|---|
| Poprawność | remis | remis | 100% we wszystkich przebiegach |
| Czas, A | **agent** 31.5 s | **agent** 32.2 s* | jcode 41.2 / 38.9 s, pi 54.3 / 46.9 s |
| Czas, B | agent 162.3 s ≈ jcode 171.1 s | **agent** 180.9 s | zakresy w B się nakładają, patrz Zastrzeżenia |
| Koszt, A | **agent** $0.09 | **agent** $0.11* | jcode $0.14 / $0.15, pi $0.20 / $0.16 |
| Koszt, B | **agent** $0.48 | **agent** $0.57 | jcode $0.60 / $0.97, pi $0.81 / $0.85 |
| Tokeny, A | **agent** 41k | **agent** 81k* | jcode 159k / 123k, pi 119k / 96k |
| Tokeny, B | **agent** 241k | **agent** 411k | jcode 399k / 734k, pi 565k / 847k |
| Wywołania narzędzi, A | **agent** 3.0 | **agent** 3.0* | jcode 4.3 / 3.8, pi 7.3 / 3.3 |
| Wywołania narzędzi, B | **agent** 9.3 | **agent** 12.0 | jcode 9.7 / 18.3, pi 16.0 / 20.0 |
| CPU, RAM, start procesu | **agent** | **agent** | 0.06 s CPU, 26 MB RSS bez pracy |
| Kontekst bazowy | **agent** 7.9k | **agent** 17.7k | jcode 26.5k / 24.2k, pi 13.2k / 19.4k |
| Rozmiar instalacji | **agent** | **agent** | 3.2 MB vs 25 MB + Node (pi) i 132 MB (jcode) |

Agent wygrywa każdą kategorię poza czasem B w `clean`, gdzie prowadzi o 9 s w granicach szumu. \* A w `real` dla agenta pochodzi z serii po zmianie jednej reguły w `~/.agents/SYSTEM.md`. Przed zmianą agent przegrywał tu wywołaniami narzędzi (5.2) i tokenami (102k): patrz "A w `real`: dwie ścieżki zapisu".

## Co zmieniono w agencie przed seriami `clean` i `real`

Seria 1 wykazała, że agent w B zużywa około 3× więcej tokenów niż jcode. Dwie przyczyny dotyczyły samego agenta (gałąź `perf/bench`, worktree `/tmp/agent-wt`, baza `ce6c23e`):

1. **Headroom Tool Search.** Proxy headroom (włączone w `real` przez `"headroom": true`) domyślnie odracza definicje narzędzi spoza swojej listy "core". Na tej liście nie ma `rg`, `replace`, `insert` ani `replace_match`. Model musiał je najpierw wyszukać przez `tool_search_tool_regex`, co dawało dodatkowe iteracje i tokeny. Teraz `headroom::sync` ustawia `HEADROOM_TOOL_SEARCH=0`, chyba że użytkownik ustawi inaczej w `headroom_env`. Reinstalacja zachowuje wartości już zapisane w manifeście (wcześniej je kasowała, np. `HEADROOM_PROTECT_TOOL_RESULTS`). Zmiana dotyczy całego serwisu headroom, nie tylko agenta. Sync uruchamia się tylko w TUI: przed serią `real` wykonano go raz przez uruchomienie TUI.
2. **Licznik kontekstu do auto-kompakcji.** Przy narzędziach serwerowych `message_delta` sumuje `usage` wszystkich iteracji. Agent brał tę sumę za rozmiar kontekstu i kompaktował za wcześnie. Teraz rozmiar kontekstu to `input + cache_read + cache_write` z `message_start` plus `output`.

Binarka obu serii: `/tmp/agent-bench2`, sha256 `29dc8fc359d46d52…`, build z `perf/bench` z niescommitowanymi zmianami.

## Co porównano

| Harness | Wersja | Wywołanie w benchmarku |
|---|---|---|
| agent | 0.1.0, `perf/bench` (patrz wyżej) | `agent --headless --yolo --no-session --model claude-opus-5-5 --thinking medium "$P"` |
| jcode | v0.98.1 (`1c7426eb1`) | `JCODE_ANTHROPIC_REASONING_EFFORT=medium jcode run --no-update --quiet -p claude -m claude-opus-5-5 --ndjson "$P"` |
| pi | 1.3.0 (`@earendil-works/pi-coding-agent`, globalnie przez bun), Node v26.12.0 | `pi -p --no-session --mode json --model anthropic/claude-opus-5-5 --thinking medium "$P"` |

Prompt `$P` był taki sam dla wszystkich:

```
Complete the work described in TASK.md in this directory. Check with `bun test`. Do not edit files under test/. Do not ask questions; work until done.
```

Środowisko: macOS 27.0.1, Apple M1 Max (10 rdzeni, 32 GB), bun 1.4.3. Każdy proces był uruchomiony przez `/usr/bin/time -l timeout 1200`, ze stdin z `/dev/null`. Kolejność harnessów rotuje co rundę (`bench/all.sh`).

### Profile

`PROFILE=clean bench/run.sh …` używa katalogów z `bench/clean-homes.sh` (w `/tmp/bench-clean`):

| Harness | Jak odcięty od konfiguracji | Co zostało |
|---|---|---|
| agent | `HOME=/tmp/bench-clean/agent` | `accounts.json`, `settings.json` z `headroom: false`, modelem, `thinking: medium` i `accounts.default` (bez tego agent bierze klucz API workspace'u zamiast subskrypcji) |
| jcode | `JCODE_HOME=/tmp/bench-clean/jcode` | `auth.json`, `auth-refresh-state.json`; domyślny `config.toml` |
| pi | `PI_CODING_AGENT_DIR=/tmp/bench-clean/pi` | `auth.json`, `settings.json` z dostawcą i modelem, **`SYSTEM.md` Artura** |

pi zachowuje `SYSTEM.md` Artura, bo z domyślnym promptem pi API odrzuca login subskrypcji: `400 Third-party apps now draw from your extra usage, not your plan limits`. W `clean` pi ma więc o jeden element konfiguracji więcej niż agent i jcode.

W `real` tylko agent przechodzi przez headroom. jcode i pi łączą się z API bezpośrednio.

## Zadania

### A: raport wydatków z CSV (`tasks/A`)

TypeScript na Bun. Trzeba naprawić parser CSV zgodnie z RFC 4180 (cudzysłowy, `""`, przecinki i nowe linie w polach, CRLF), napisać `parseAmount` liczący dokładnie w groszach, dodać do `summarize` liczenie pominiętych wierszy i napisać CLI `src/cli.ts`. Specyfikacja jest w `tasks/A/fixture/TASK.md`.

- Fixture: `tasks/A/fixture`. Na starcie 0/3 widocznych testów przechodzi.
- Ukryte testy: `tasks/A/hidden.test.ts` (36 testów + 3 widoczne = 39). Przed oceną kopiowane do `test/`.
- Referencja: `tasks/A/reference/src` (39/39).

### B: zakresy semver (`tasks/B`)

Zadanie wymaga napisania od zera `satisfies` i `maxSatisfying` zgodnych z node-semver v7, bez zależności. Specyfikacja jest w `tasks/B/fixture/TASK.md`.

- Fixture: `tasks/B/fixture`, z naiwną implementacją, która daje 1979/2550.
- Grader: `tasks/B/grade.ts` porównuje z `tasks/B/cases.json`. Plik ma 2550 przypadków: 2193 `satisfies` i 357 `maxSatisfying`.
- Oracle: `tasks/B/oracle/gen.ts` generuje `cases.json` z prawdziwego `semver@7.8.5`. Generator jest deterministyczny: ponowne uruchomienie daje identyczny bajtowo plik.
- Referencja: `tasks/B/reference/semver.ts` (2550/2550).

## Wyniki

Średnie, w nawiasach zakres. Liczba przebiegów jest przy wyniku (×3 lub ×6). Dane per przebieg: `bench/parse.sh results/<profil>/<A|B>`.

### Profil `clean`

| | agent | jcode | pi |
|---|---|---|---|
| A: wynik | 39/39 ×3 | 39/39 ×3 | 39/39 ×3 |
| A: czas | **31.5 s** (27.8–37.2) | 41.2 s (38.1–44.3) | 54.3 s (40.6–62.9) |
| A: wywołania narzędzi | **3.0** (3–3) | 4.3 (4–5) | 7.3 (5–11) |
| A: tokeny łącznie | **41.4k** | 159.1k | 118.6k |
| A: koszt | **$0.09** | $0.14 | $0.20 |
| B: wynik | 2550/2550 ×6 | 2550/2550 ×6 | 2550/2550 ×3 |
| B: czas | **162.3 s** (116.4–230.1) | 171.1 s (135.2–239.1) | 223.0 s (207.6–236.0) |
| B: wywołania narzędzi | **9.3** (4–14) | 9.7 (5–23) | 16.0 (13–18) |
| B: tokeny wyjściowe | **14690** | 18707 | 22809 |
| B: tokeny łącznie | **241.3k** | 399.2k | 564.8k |
| B: koszt | **$0.48** | $0.60 | $0.81 |
| B: dodane linie w `src/` | 262 | 390 | 339 |

### Profil `real`

| | agent | jcode | pi |
|---|---|---|---|
| A: wynik | 39/39 ×6 | 39/39 ×6 | 39/39 ×6 |
| A: czas | 36.9 s (27.9–58.4) | 38.9 s (27.9–48.1) | 46.9 s (41.8–55.9) |
| A: wywołania narzędzi | 5.2 (3–9) | 3.8 (3–6) | 3.3 (3–4) |
| A: tokeny łącznie | 101.9k | 123.3k | 96.0k |
| A: koszt | $0.11 | $0.15 | $0.16 |
| A, agent po zmianie reguły: wynik | 39/39 ×6 | | |
| A, agent po zmianie reguły: czas | **32.2 s** (27.4–45.9) | | |
| A, agent po zmianie reguły: wywołania narzędzi | **3.0** (3–3) | | |
| A, agent po zmianie reguły: tokeny łącznie | **80.7k** | | |
| A, agent po zmianie reguły: koszt | **$0.11** | | |
| B: wynik | 2550/2550 ×3 | 2550/2550 ×3 | 2550/2550 ×3 |
| B: czas | **180.9 s** (127.7–263.5) | 272.8 s (213.2–375.4) | 252.3 s (166.4–326.6) |
| B: wywołania narzędzi | **12.0** (7–16) | 18.3 (11–29) | 20.0 (10–27) |
| B: tokeny wyjściowe | **16635** | 27970 | 19728 |
| B: tokeny łącznie | **411.3k** | 734.4k | 847.2k |
| B: koszt | **$0.57** | $0.97 | $0.85 |
| B: dodane linie w `src/` | 287 | 301 | 296 |

### A w `real`: dwie ścieżki zapisu

Przebiegi A agenta w `real` idą jedną z dwóch ścieżek. Na 6 przebiegów 4 zapisały pliki heredokiem w `bash` (3–4 wywołania, ~30 s, $0.09–0.11). 2 użyły narzędzia `write` (9 wywołań, 42–58 s, $0.14–0.15). W `clean` wszystkie 3 przebiegi poszły ścieżką `bash`. pi w `real` pisze tylko przez `bash`. jcode w 4 z 6 przebiegów robi to samo, w 2 dokłada `edit` lub `apply_patch`.

Ścieżkę `write` wybiera model, a powodem jest prompt, nie harness. `~/.agents/SYSTEM.md` Artura zawiera regułę "Project files change only through the file tools: `write` creates a file, the anchored tools edit an existing one". `~/.pi/agent/SYSTEM.md` to inna wersja tego pliku, więc pi w `real` dostaje inny prompt niż agent.

Ścieżka `write` kosztuje więcej, bo:
- cztery pliki to cztery wywołania `write` (w jednej turze, równolegle), a nie jeden `bash`;
- po zapisie model poprawia kod kotwicowymi `replace` (np. komparator sortowania, `-0`), co daje 2–3 dodatkowe tury. Każda tura wczytuje cały kontekst z cache.

Diagnostyka LSP po `write` dodaje szum: `tsc-rs` bez `@types/bun` zgłasza `Cannot find name 'Bun'` i `'process'` (TS2591, TS2868). Ścieżki sprawdzono w 4 dodatkowych przebiegach z zapisem sesji, które nie są w wynikach. 3 z nich poszły ścieżką `write`, i w żadnym model nie zmienił kodu z powodu tych błędów.

Zmiana reguły: "Change part of a project file with the anchored tools (...). Create or fully rewrite a file with `write`, or several files at once with one `bash` heredoc." Po niej 6/6 przebiegów agenta poszło ścieżką `bash`: 3 wywołania, 32.2 s, 80.7k tokenów (`results/real/A-new-rule`). B w `real` i przebiegi pi oraz jcode nie były powtarzane. Ich prompty tej reguły nie zawierają.

Max RSS i CPU z `/usr/bin/time -l` obejmują procesy potomne, w tym `bun test`. W przebiegach z pracą mierzą więc głównie testy, nie harness. Czyste koszty harnessu pokazuje tabela "Narzut bez pracy".

### Narzut bez pracy (seria 1, 5 przebiegów, prompt "Reply with just OK", profil `real`)

| | agent | jcode | pi |
|---|---|---|---|
| Czas (zakres) | **2.17–3.59 s**, mediana 2.57 | 2.97–4.14 s, mediana 3.30 | 3.07–5.28 s, mediana 3.23 |
| CPU procesu | **0.06–0.07 s** | 0.19–0.21 s | 1.81–2.06 s |
| Max RSS | **26–27 MB** | 47–48 MB | 308–314 MB |

Kontekst bazowy (suma `input`, `cache_read` i `cache_write` pierwszego żądania, prompt "Reply OK"):

| | agent | jcode | pi |
|---|---|---|---|
| `clean` | **7.9k** | 26.5k | 13.2k |
| `real` | **17.7k** | 24.2k | 19.4k |

Zmierzone binarką `/tmp/agent-bench2` po serii `real`. Agent w `real` ma 17.7k zamiast 12.4k z serii 1, bo bez Tool Search headroom wysyła pełne definicje narzędzi.

### Uczciwość przebiegów

We wszystkich 51 przebiegach serii `clean` i `real`:

- Pliki `test-tamper.diff` są puste, więc nikt nie ruszał `test/`.
- Pliki `pkg.diff` w B są puste, więc nikt nie dodał zależności.

jcode w obu profilach sam commituje swoją pracę. `run.sh` liczył wcześniej diffy względem `HEAD`, więc dla jcode `final.patch` i `test-tamper.diff` wychodziły puste niezależnie od zmian. Teraz liczy względem commita `init` (`$BASE`). Pliki w `results/clean` przeliczono po tej poprawce z zachowanych katalogów roboczych. Seria 1 tego problemu nie miała: jcode wtedy nie commitował.

## Seria 1 (historyczna)

Pierwsza seria, profil `real`, binarka agenta sprzed zmian i z Tool Search w headroom. Dane w `results/series1`.

| | agent | jcode | pi |
|---|---|---|---|
| A: czas | 42.1 s (29.6–53.1) | **33.0 s** (30.6–37.5) | 43.5 s (40.2–46.2) |
| A: koszt | **$0.11** | **$0.11** | $0.15 |
| B: czas | 204.7 s (188.8–235.9) | **150.2 s** (134.4–170.6) | 251.0 s (214.9–282.8) |
| B: wywołania narzędzi | 17.3 (6–27) | **6.3** (4–10) | 21.0 (16–24) |
| B: tokeny łącznie | 695.7k | **237.2k** | 712.8k |
| B: koszt | $0.87 | **$0.55** | $0.79 |

Ta sama binarka przed zmianami, ten sam profil (seria pośrednia, niepublikowana): agent B 223.7 s, 18.0 wywołań, 749k tokenów, $0.95.

W serii 1 kolejność w A była stała (agent → jcode → pi), a agent był przebudowywany w trakcie pomiarów (commity `0cc4edd`…`15c31fb`). Pierwsza wersja `run.sh` nie tworzyła katalogu roboczego, więc pierwszy przebieg agenta wystartował w `~` z `--yolo` i został zabity po około 14 s. W repozytoriach w `~` nie znaleziono zmian. Poprawka: `mkdir -p` i `cd "$D" || exit 1` przed uruchomieniem harnessu.

## Zastrzeżenia

1. **Duży rozrzut w B.** Czas jednego harnessu w B waha się nawet 2× (agent `real`: 127.7–263.5 s, jcode `real`: 213.2–375.4 s). Przy 3–6 przebiegach różnice średnich czasu w B poniżej około 50 s są szumem. Różnice tokenów i kosztu są stabilniejsze.
2. **jcode nie jest stabilny między seriami.** W serii 1 jcode zrobił B w 6.3 wywołania i 237k tokenów, w `real` w 18.3 i 734k. Ta sama wersja i konfiguracja. Przyczyny nie badano.
3. **Effort w jcode nie jest potwierdzony.** `config.toml` jcode ma `anthropic_reasoning_effort = "high"`. Zmienna `JCODE_ANTHROPIC_REASONING_EFFORT` istnieje w binarce, ale nie sprawdzono, czy nadpisuje config w wysyłanym żądaniu.
4. **pi w `clean` ma system prompt Artura** (patrz "Profile").
5. **Koszt to ekwiwalent API**, nie realny wydatek na subskrypcji. Cennik `claude-opus-5-5` za 1M tokenów: input $4, output $20, cache read $0.20, cache write $5.
6. **Jakości kodu nie oceniano** poza testami. Diffy leżą w `results/*/*/*/final.patch`.
7. **Zmiany w agencie nie są scommitowane.** Liczby agenta dotyczą builda z `/tmp/agent-wt` (gałąź `perf/bench`).

## Struktura katalogu

```
analiza/
  README.md                 ten raport
  bench/
    run.sh                  jeden przebieg: [PROFILE=clean] bench/run.sh <A|B> <agent|jcode|pi> <id>
    all.sh                  pełna seria, rotacja kolejności: bench/all.sh [N=3]
    clean-homes.sh          buduje /tmp/bench-clean dla PROFILE=clean
    idle.sh                 narzut bez pracy: bench/idle.sh [N=5]
    parse.sh                metryki: bench/parse.sh <katalog-wyników>
    original/               skrypty w wersji z serii 1 (ścieżki /tmp/hb)
  tasks/
    A/fixture/              stan startowy zadania A (TASK.md, src/, test/, data/)
    A/hidden.test.ts        ukryte testy A
    A/reference/src/        rozwiązanie referencyjne A
    B/fixture/              stan startowy zadania B
    B/grade.ts, cases.json  grader i 2550 przypadków
    B/oracle/               generator przypadków z semver@7 (bun install && bun gen.ts)
    B/reference/semver.ts   rozwiązanie referencyjne B
  results/
    clean/, real/, series1/  (real/A-new-rule: agent A po zmianie reguły w SYSTEM.md)
      A/<harness>-<n>/, B/<harness>-<n>/
        events.jsonl        pełny strumień zdarzeń harnessu (stdout)
        stderr.txt          stderr + raport /usr/bin/time -l (real/user/sys, max RSS)
        wall, exit          czas ścienny w s i kod wyjścia
        grade.txt           wynik ukrytej oceny
        final.patch         końcowy diff względem fixture'u (bez plików oceny)
        diffstat, status    git diff --stat i git status po przebiegu
        test-tamper.diff    zmiany w test/ (puste = brak)
        pkg.diff, ls, visible.txt   tylko B: zmiany w package.json, lista plików, widoczne testy
    series1/idle/<harness>-<n>.out|.err   przebiegi bez pracy
```

## Jak powtórzyć

```bash
cd ~/developer/agent/analiza
bench/clean-homes.sh                                        # tylko przed PROFILE=clean
PROFILE=clean AGENT_BIN=/ścieżka/agent bench/all.sh 3       # WORK=/tmp/hb-work, OUT=/tmp/hb-out domyślnie
AGENT_BIN=/ścieżka/agent bench/all.sh 3                     # profil real
bench/parse.sh /tmp/hb-out/A; bench/parse.sh /tmp/hb-out/B
bench/idle.sh 5
```

Pełna seria trwa 35–45 minut. Każdy harness działa z `--yolo` lub odpowiednikiem w osobnym katalogu w `$WORK`. `run.sh` kończy działanie, jeśli nie może do niego wejść.

Przed serią `real` z headroom uruchom raz TUI agenta, żeby `headroom::sync` ustawił `HEADROOM_TOOL_SEARCH=0` w serwisie. Sprawdzenie: `jq .base_env.HEADROOM_TOOL_SEARCH ~/.headroom/deploy/default/manifest.json`.

Odtworzenie `cases.json`: `cd tasks/B/oracle && bun install && bun gen.ts`. Plik zostanie nadpisany identycznym.

## Formaty zdarzeń (dla `parse.sh`)

| Harness | Tokeny | Wywołania narzędzi |
|---|---|---|
| agent | jedno zdarzenie `{"type":"usage","input","output","cache_read","cache_write"}` na koniec, skumulowane | `{"type":"tool"}`, tylko nazwa, bez argumentów |
| jcode | `{"type":"tokens","input","output","cache_read_input","cache_creation_input"}` na każdą odpowiedź modelu; trzeba sumować | `{"type":"tool_start"}` |
| pi | `message_end` z `message.role=="assistant"` i `message.usage.{input,output,cacheRead,cacheWrite}` na każdą odpowiedź; trzeba sumować | elementy `message.content[]` z `type=="toolCall"` |

## Możliwe dalsze kroki

- 5–10 przebiegów na zadanie, żeby rozstrzygnąć czas B. Trzecie zadanie z nawigacją po dużym, istniejącym repozytorium.
- Uruchamiać `headroom::sync` także w trybie headless, żeby benchmark nie zależał od wcześniejszego uruchomienia TUI.
- Ustalić effort jcode z logu żądania.
- Zdarzenie `tool` w trybie headless agenta mogłoby zawierać argumenty, żeby przebieg dało się w pełni odtworzyć.
