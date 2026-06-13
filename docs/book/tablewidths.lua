-- tablewidths.lua — force every table column to carry an explicit relative
-- width so the LaTeX/PDF writer emits wrapping p{} columns instead of fixed
-- l-columns. Without this, long descriptive cells in the mapping tables run
-- off the right margin (overfull \hbox). GFM tables otherwise arrive with no
-- width hints, so we distribute the available width across columns in
-- proportion to each column's widest cell (so a long "description" column gets
-- most of the line and a short "crate name" column stays narrow).

local function cell_len(cell)
  return #pandoc.utils.stringify(cell)
end

-- Widest cell length seen in a given column index across header + body rows.
local function column_weights(tbl, n)
  local weights = {}
  for i = 1, n do weights[i] = 1 end

  local function scan(rows)
    for _, row in ipairs(rows) do
      for i = 1, n do
        local c = row.cells[i]
        if c then
          local len = cell_len(c)
          if len > weights[i] then weights[i] = len end
        end
      end
    end
  end

  scan(tbl.head.rows)
  for _, body in ipairs(tbl.bodies) do
    scan(body.body)
  end
  return weights
end

function Table(tbl)
  local colspecs = tbl.colspecs
  local n = #colspecs
  if n == 0 then return nil end

  local weights = column_weights(tbl, n)
  local total = 0
  for i = 1, n do total = total + weights[i] end
  if total == 0 then return nil end

  -- Sum to ~0.96 to leave room for inter-column padding/rules.
  for i = 1, n do
    local align = colspecs[i][1]
    colspecs[i] = { align, 0.96 * (weights[i] / total) }
  end
  tbl.colspecs = colspecs
  return tbl
end
