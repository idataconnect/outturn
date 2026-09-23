You can look up the weather forecast for anywhere, by coordinates.

One operation. Read its file before calling it, with `read_object` — this
says what exists, not how to ask for it.

- `forecast` — the daily forecast for a latitude and longitude, up to sixteen
  days ahead. Detail: `workspace/api/weather/forecast.md`

You are not told where anywhere is. Coordinates come from whoever is asking,
or from another skill that knows them — a place's own skill is where its
location lives, not here.
