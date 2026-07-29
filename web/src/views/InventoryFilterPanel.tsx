import type { ReactNode } from 'react'
import type { InventoryFilters, MinPlat, PartKind, Tri } from './inventoryFilters'
import { toggleMinPlat, togglePartKind, toggleTri } from './inventoryFilters'

type T = (k: string, vars?: Record<string, string | number>) => string

function FilterGroup({
  label,
  children,
}: {
  label: string
  children: ReactNode
}) {
  return (
    <div className="inv-filter-group">
      <div className="inv-filter-label">{label}</div>
      <div className="inv-filter-opts">{children}</div>
    </div>
  )
}

function Opt({
  active,
  onClick,
  children,
}: {
  active: boolean
  onClick: () => void
  children: ReactNode
}) {
  return (
    <button type="button" className={`inv-filter-opt${active ? ' active' : ''}`} onClick={onClick}>
      {children}
    </button>
  )
}

export function InventoryFilterPanel({
  filters,
  onChange,
  t,
}: {
  filters: InventoryFilters
  onChange: (next: InventoryFilters) => void
  t: T
}) {
  const setTri = (key: keyof InventoryFilters, next: boolean) => {
    const cur = filters[key] as Tri
    onChange({ ...filters, [key]: toggleTri(cur, next) })
  }

  return (
    <aside className="inv-filters">
      <FilterGroup label={t('filt_crafted')}>
        <Opt active={filters.mastered === true} onClick={() => setTri('mastered', true)}>
          {t('filt_mastered')}
        </Opt>
        <Opt active={filters.mastered === false} onClick={() => setTri('mastered', false)}>
          {t('filt_unmastered')}
        </Opt>
      </FilterGroup>

      <FilterGroup label={t('filt_owned_gt1')}>
        <Opt active={filters.ownedGt1 === true} onClick={() => setTri('ownedGt1', true)}>
          {t('yes')}
        </Opt>
        <Opt active={filters.ownedGt1 === false} onClick={() => setTri('ownedGt1', false)}>
          {t('no')}
        </Opt>
      </FilterGroup>

      <FilterGroup label={t('filt_vaulted')}>
        <Opt active={filters.vaulted === true} onClick={() => setTri('vaulted', true)}>
          {t('yes')}
        </Opt>
        <Opt active={filters.vaulted === false} onClick={() => setTri('vaulted', false)}>
          {t('no')}
        </Opt>
      </FilterGroup>

      <FilterGroup label={t('filt_order')}>
        <Opt active={filters.orderPlaced === true} onClick={() => setTri('orderPlaced', true)}>
          {t('yes')}
        </Opt>
        <Opt active={filters.orderPlaced === false} onClick={() => setTri('orderPlaced', false)}>
          {t('no')}
        </Opt>
      </FilterGroup>

      <FilterGroup label={t('filt_part_type')}>
        <Opt
          active={filters.partKind === 'normal'}
          onClick={() =>
            onChange({
              ...filters,
              partKind: togglePartKind(filters.partKind, 'normal') as PartKind,
            })
          }
        >
          {t('filt_normal')}
        </Opt>
        <Opt
          active={filters.partKind === 'prime'}
          onClick={() =>
            onChange({
              ...filters,
              partKind: togglePartKind(filters.partKind, 'prime') as PartKind,
            })
          }
        >
          {t('filt_prime')}
        </Opt>
      </FilterGroup>

      <FilterGroup label={t('filt_favorite')}>
        <Opt active={filters.favorite === true} onClick={() => setTri('favorite', true)}>
          {t('yes')}
        </Opt>
        <Opt active={filters.favorite === false} onClick={() => setTri('favorite', false)}>
          {t('no')}
        </Opt>
      </FilterGroup>

      <FilterGroup label={t('filt_min_plat')}>
        {([5, 10, 15] as const).map((n) => (
          <Opt
            key={n}
            active={filters.minPlat === n}
            onClick={() =>
              onChange({
                ...filters,
                minPlat: toggleMinPlat(filters.minPlat, n) as MinPlat,
              })
            }
          >
            {n}
          </Opt>
        ))}
      </FilterGroup>

      <FilterGroup label={t('filt_set_complete')}>
        <Opt active={filters.setComplete === true} onClick={() => setTri('setComplete', true)}>
          {t('yes')}
        </Opt>
        <Opt active={filters.setComplete === false} onClick={() => setTri('setComplete', false)}>
          {t('no')}
        </Opt>
      </FilterGroup>
    </aside>
  )
}
