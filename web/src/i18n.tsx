import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useState,
  type ReactNode,
} from 'react'

export type Locale = 'ru' | 'en'

const STORAGE_KEY = 'bngframe.locale'

const dict = {
  ru: {
    tagline:
      'Компаньон в духе AlecaFrame для Linux Wayland — оверлеи по EE.log, инвентарь через memory JWT, warframe.market, реликвии и ривены.',
    daemon: 'Демон',
    tab_overview: 'Обзор',
    tab_inventory: 'Инвентарь',
    tab_relics: 'Реликвии',
    tab_market: 'Маркет',
    tab_rivens: 'Ривены',
    tab_stats: 'Статистика',
    tab_analytics: 'Аналитика',
    tab_settings: 'Настройки',

    eelog: 'EE.log',
    found: 'найден',
    missing: 'нет файла',
    items_cached: 'Предметов в кэше',
    inventory: 'Инвентарь',
    loaded: 'загружен',
    empty: 'пусто',
    watching: 'Слежение',
    yes: 'да',
    no: 'нет',

    trigger_ocr: 'Запустить OCR наград',
    refresh_items: 'Обновить каталог маркета',
    refresh_prices: 'Подтянуть цены инвентаря',
    prices_hint: 'Каталог — только названия. Цены качаются отдельно с warframe.market (лимит за раз).',
    open_overlay: 'Открыть оверлей',
    last_rewards: 'Последние награды',
    no_rewards: 'Наград ещё нет. Завершите fissure или запустите OCR вручную.',
    source: 'источник',

    sync_memory: 'Синхронизация из памяти игры',
    import_dump: 'Импорт дампа',
    filter_placeholder: 'Фильтр…',
    view_all: 'Все',
    view_mastery: 'Помощник мастерства',
    view_foundry: 'Литейная / чертежи',
    duplicates: 'дубликаты',
    tradable_guess: 'похоже на торговлю',
    inventory_hint:
      'Инвентарь кэшируется локально (SQLite + inventory_cache.json). Sync из памяти нужен только для обновления; избранное сохраняется.',
    cache_label: 'Кэш',
    cache_empty: 'кэша нет — сделайте sync',
    cache_age: 'обновлён {age}',
    cache_just_now: 'только что',
    cache_minutes: '{n} мин назад',
    cache_hours: '{n} ч назад',
    cache_days: '{n} дн назад',
    col_name: 'Название',
    col_type: 'Тип',
    col_qty: 'Кол-во',
    col_plat: 'Платина',
    col_mastered: 'Мастерство',
    showing: 'Показано {shown} из {total}',

    order_ducats: 'Прибыль дукатов',
    order_platinum: 'Платина',
    order_mr: 'Лучше для MR',
    favorites_first: 'избранное сверху',
    apply_planner: 'Применить к планировщику / оверлею',
    col_relic: 'Реликвия',
    col_tier: 'Тир',
    col_owned: 'Есть',
    col_score: 'Оценка',
    col_top_drop: 'Топ дроп',

    sign_in: 'Вход',
    email: 'email',
    password: 'пароль',
    sign_in_btn: 'Войти',
    jwt_placeholder: 'Или вставьте JWT cookie',
    save_jwt: 'Сохранить JWT',
    your_orders: 'Ваши ордера',
    col_item: 'Предмет',
    col_order_type: 'Тип',
    listing_suggestions: 'Предложения листингов (дубликаты)',
    col_suggested: 'Цена',
    list_btn: 'Выставить',

    analyze: 'Анализ',
    analyze_btn: 'Анализировать',
    compare_rerolls: 'Сравнение рероллов',
    compare_btn: 'Сравнить',

    credits: 'Кредиты',
    platinum: 'Платина',
    trades_logged: 'Сделок записано',
    history: 'История',

    top_market_cap: 'Топ market cap (кэш цен)',
    col_vol: 'Объём',
    col_cap: 'Cap',
    ocr_lang_matrix: 'Матрица языков OCR',

    lang_label: 'Язык интерфейса',
    consent:
      'Согласие на инвентарь (скан памяти / мобильный API DE) — серая зона относительно Overwolf; на свой риск',
    overlay_enabled: 'Оверлей включён',
    ocr_lang: 'Язык OCR',
    eelog_path: 'Путь к EE.log',
    save_settings: 'Сохранить настройки',
  },
  en: {
    tagline:
      'AlecaFrame-style companion for Linux Wayland — EE.log overlays, inventory via memory JWT, warframe.market, relics & rivens.',
    daemon: 'Daemon',
    tab_overview: 'Overview',
    tab_inventory: 'Inventory',
    tab_relics: 'Relics',
    tab_market: 'Market',
    tab_rivens: 'Rivens',
    tab_stats: 'Stats',
    tab_analytics: 'Analytics',
    tab_settings: 'Settings',

    eelog: 'EE.log',
    found: 'found',
    missing: 'missing',
    items_cached: 'Items cached',
    inventory: 'Inventory',
    loaded: 'loaded',
    empty: 'empty',
    watching: 'Watching',
    yes: 'yes',
    no: 'no',

    trigger_ocr: 'Trigger reward OCR',
    refresh_items: 'Refresh market catalog',
    refresh_prices: 'Fetch inventory prices',
    prices_hint: 'Catalog is names only. Prices are fetched separately from warframe.market (rate-limited).',
    open_overlay: 'Open overlay',
    last_rewards: 'Last rewards',
    no_rewards: 'No rewards yet. Finish a fissure or trigger OCR manually.',
    source: 'source',

    sync_memory: 'Sync from game memory',
    import_dump: 'Import dump',
    filter_placeholder: 'Filter…',
    view_all: 'All',
    view_mastery: 'Mastery helper',
    view_foundry: 'Foundry / blueprints',
    duplicates: 'duplicates',
    tradable_guess: 'tradable guess',
    inventory_hint:
      'Inventory is cached locally (SQLite + inventory_cache.json). Memory sync only refreshes it; favorites are kept.',
    cache_label: 'Cache',
    cache_empty: 'no cache — run a sync',
    cache_age: 'updated {age}',
    cache_just_now: 'just now',
    cache_minutes: '{n} min ago',
    cache_hours: '{n} h ago',
    cache_days: '{n} d ago',
    col_name: 'Name',
    col_type: 'Type',
    col_qty: 'Qty',
    col_plat: 'Plat',
    col_mastered: 'Mastered',
    showing: 'Showing {shown} of {total}',

    order_ducats: 'Ducats profit',
    order_platinum: 'Platinum',
    order_mr: 'Best for MR',
    favorites_first: 'favorites first',
    apply_planner: 'Apply to planner / overlay',
    col_relic: 'Relic',
    col_tier: 'Tier',
    col_owned: 'Owned',
    col_score: 'Score',
    col_top_drop: 'Top drop',

    sign_in: 'Sign in',
    email: 'email',
    password: 'password',
    sign_in_btn: 'Sign in',
    jwt_placeholder: 'Or paste JWT cookie',
    save_jwt: 'Save JWT',
    your_orders: 'Your orders',
    col_item: 'Item',
    col_order_type: 'Type',
    listing_suggestions: 'Listing suggestions (duplicates)',
    col_suggested: 'Suggested',
    list_btn: 'List',

    analyze: 'Analyze',
    analyze_btn: 'Analyze',
    compare_rerolls: 'Compare rerolls',
    compare_btn: 'Compare',

    credits: 'Credits',
    platinum: 'Platinum',
    trades_logged: 'Trades logged',
    history: 'History',

    top_market_cap: 'Top market cap (cached prices)',
    col_vol: 'Vol',
    col_cap: 'Cap',
    ocr_lang_matrix: 'OCR language matrix',

    lang_label: 'UI language',
    consent:
      'Inventory consent (memory scrape / DE mobile API) — gray-area vs Overwolf; use at your own risk',
    overlay_enabled: 'Overlay enabled',
    ocr_lang: 'OCR lang',
    eelog_path: 'EE.log path',
    save_settings: 'Save settings',
  },
} as const

export type MessageKey = keyof typeof dict.ru

type I18nCtx = {
  locale: Locale
  setLocale: (l: Locale) => void
  t: (key: MessageKey, vars?: Record<string, string | number>) => string
}

const Ctx = createContext<I18nCtx | null>(null)

function detectInitial(): Locale {
  try {
    const saved = localStorage.getItem(STORAGE_KEY)
    if (saved === 'ru' || saved === 'en') return saved
  } catch {
    /* ignore */
  }
  const nav = (navigator.language || '').toLowerCase()
  if (nav.startsWith('ru') || nav.startsWith('uk') || nav.startsWith('be')) return 'ru'
  return 'ru' // default Russian for this project
}

export function I18nProvider({ children }: { children: ReactNode }) {
  const [locale, setLocaleState] = useState<Locale>(detectInitial)

  const setLocale = useCallback((l: Locale) => {
    setLocaleState(l)
    try {
      localStorage.setItem(STORAGE_KEY, l)
    } catch {
      /* ignore */
    }
    document.documentElement.lang = l === 'ru' ? 'ru' : 'en'
  }, [])

  useEffect(() => {
    document.documentElement.lang = locale === 'ru' ? 'ru' : 'en'
  }, [locale])

  const t = useCallback(
    (key: MessageKey, vars?: Record<string, string | number>) => {
      let s: string = dict[locale][key] ?? dict.en[key] ?? key
      if (vars) {
        for (const [k, v] of Object.entries(vars)) {
          s = s.replace(`{${k}}`, String(v))
        }
      }
      return s
    },
    [locale],
  )

  const value = useMemo(() => ({ locale, setLocale, t }), [locale, setLocale, t])
  return <Ctx.Provider value={value}>{children}</Ctx.Provider>
}

export function useI18n() {
  const ctx = useContext(Ctx)
  if (!ctx) throw new Error('useI18n outside provider')
  return ctx
}
