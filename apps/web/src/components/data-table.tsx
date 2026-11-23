import { ChevronDown } from "reicon-react";
import { type ReactNode, useState } from "react";
import { Link } from "react-router";
import { Skeleton } from "@/components/ui";
import { cn } from "@/lib/utils";

export type SortDirection = "asc" | "desc";
export type SortState<K extends string> = { key: K; direction: SortDirection } | null;
export type SortValue = string | number | null | undefined;

export type Column<T, K extends string = string> = {
  key: K;
  header: ReactNode;
  /** The header name for screen readers when `header` is not plain text. */
  label?: string;
  cell: (row: T) => ReactNode;
  /** A sort value makes the header sortable here. `true` makes it sortable while the caller sorts the rows (server side). */
  sort?: ((row: T) => SortValue) | true;
  /** Classes for the header and every cell of the column, such as responsive visibility. */
  className?: string;
  /** Classes for the body cells only. */
  cellClassName?: string;
  /** Gets clicks on the cell to itself, so they do not open the row. */
  stopRowClick?: boolean;
};

/** Empty values sort last in both directions. */
export function sortRows<T, K extends string>(rows: T[], columns: Column<T, K>[], sort: SortState<K>) {
  const value = columns.find((column) => column.key === sort?.key)?.sort;
  if (!sort || typeof value !== "function") return rows;
  return [...rows].sort((a, b) => {
    const left = value(a);
    const right = value(b);
    if (left == null || left === "") return right == null || right === "" ? 0 : 1;
    if (right == null || right === "") return -1;
    const order =
      typeof left === "number" && typeof right === "number"
        ? left - right
        : String(left).localeCompare(String(right), undefined, { numeric: true, sensitivity: "base" });
    return sort.direction === "asc" ? order : -order;
  });
}

/** The first click on a column sorts ascending, the next one flips it. */
export function nextSort<K extends string>(current: SortState<K>, key: K): SortState<K> {
  return current?.key === key ? { key, direction: current.direction === "asc" ? "desc" : "asc" } : { key, direction: "asc" };
}

type Props<T, K extends string> = {
  columns: Column<T, K>[];
  rows: T[];
  rowKey: (row: T, index: number) => string;
  /** Opens the row as a link. The caller puts a `row-link` Link in the first cell for keyboard users. */
  rowLink?: (row: T) => string;
  onRowClick?: (row: T) => void;
  rowSelected?: (row: T) => boolean;
  loading?: boolean;
  /** The sort when the table owns it. */
  initialSort?: SortState<K>;
  /** The sort when the caller owns it, for a persisted or server-side sort. */
  sort?: SortState<K>;
  onSort?: (key: K) => void;
  hideHeader?: boolean;
  className?: string;
};

export function DataTable<T, K extends string>({
  columns,
  rows,
  rowKey,
  rowLink,
  onRowClick,
  rowSelected,
  loading,
  initialSort = null,
  sort: controlledSort,
  onSort,
  hideHeader,
  className,
}: Props<T, K>) {
  const [ownSort, setOwnSort] = useState<SortState<K>>(initialSort);
  const sort = controlledSort === undefined ? ownSort : controlledSort;
  const toggle = onSort ?? ((key: K) => setOwnSort((current) => nextSort(current, key)));
  const sorted = sortRows(rows, columns, sort);
  const clickable = Boolean(rowLink || onRowClick);

  return (
    <div className="overflow-x-auto">
      <table className={cn("cool-table", className)}>
        {hideHeader ? null : (
          <thead>
            <tr>
              {columns.map((column) => (
                <HeaderCell key={column.key} column={column} sort={sort} onSort={toggle} />
              ))}
            </tr>
          </thead>
        )}
        <tbody>
          {loading ? <SkeletonRows columns={columns.map((column) => cn(column.className, column.cellClassName) || undefined)} /> : null}
          {sorted.map((row, index) => {
            const to = rowLink?.(row);
            return (
              <tr
                key={rowKey(row, index)}
                className={clickable ? "clickable-row" : undefined}
                aria-selected={rowSelected ? rowSelected(row) : undefined}
                onClick={onRowClick ? () => onRowClick(row) : undefined}
              >
                {columns.map((column) => (
                  <td
                    key={column.key}
                    className={cn(to && "cell-link", column.className, column.cellClassName)}
                    onClick={column.stopRowClick ? (event) => event.stopPropagation() : undefined}
                  >
                    {to ? <Link className="cell-link-anchor" to={to} tabIndex={-1} aria-hidden="true" /> : null}
                    {column.cell(row)}
                  </td>
                ))}
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}

function HeaderCell<T, K extends string>({ column, sort, onSort }: { column: Column<T, K>; sort: SortState<K>; onSort: (key: K) => void }) {
  if (!column.sort) return <th className={column.className}>{column.header}</th>;
  const direction = sort?.key === column.key ? sort.direction : null;
  const name = column.label ?? (typeof column.header === "string" ? column.header : column.key);
  return (
    <th className={column.className} aria-sort={direction === "asc" ? "ascending" : direction === "desc" ? "descending" : undefined}>
      <button
        type="button"
        className={cn("table-sort-header", direction && "is-sorted", direction === "desc" && "is-desc")}
        aria-label={`Sort by ${name}${direction ? `, sorted ${direction === "asc" ? "ascending" : "descending"}` : ""}`}
        onClick={() => onSort(column.key)}
      >
        {column.header}
        <ChevronDown className="table-sort-icon" aria-hidden="true" />
      </button>
    </th>
  );
}

const skeletonWidths = ["w-40", "w-16", "w-24", "w-20", "w-14", "w-28"];

/** Placeholder rows with the column count of the loaded table, so the table does not jump when data arrives. */
function SkeletonRows({ columns, rows = 4 }: { columns: (string | undefined)[]; rows?: number }) {
  return (
    <>
      {Array.from({ length: rows }, (_, row) => (
        <tr key={row} aria-hidden="true">
          {columns.map((className, column) => (
            <td key={column} className={className}>
              <Skeleton className={cn("h-3", skeletonWidths[column % skeletonWidths.length], column === 0 && row % 2 === 1 && "w-32")} />
            </td>
          ))}
        </tr>
      ))}
    </>
  );
}
