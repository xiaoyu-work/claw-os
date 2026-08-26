import { useState, useMemo } from 'react';
import { ArrowUpDown, ArrowLeftRight } from 'lucide-react';

interface Category { name: string; units: Record<string, number> }

const CATEGORIES: Record<string, Category> = {
  Length: {
    name: 'Length',
    units: { Meter: 1, Kilometer: 1000, Centimeter: 0.01, Millimeter: 0.001, Mile: 1609.34, Yard: 0.9144, Foot: 0.3048, Inch: 0.0254 },
  },
  Weight: {
    name: 'Weight',
    units: { Kilogram: 1, Gram: 0.001, Milligram: 1e-6, Pound: 0.453592, Ounce: 0.0283495, Ton: 907.185 },
  },
  Temperature: {
    name: 'Temperature',
    units: { Celsius: 1, Fahrenheit: 1, Kelvin: 1 },
  },
  Volume: {
    name: 'Volume',
    units: { Liter: 1, Milliliter: 0.001, Gallon: 3.78541, Quart: 0.946353, Pint: 0.473176, Cup: 0.236588, Fluid_Ounce: 0.0295735 },
  },
  Area: {
    name: 'Area',
    units: { Square_Meter: 1, Square_Kilometer: 1e6, Square_Mile: 2.59e6, Acre: 4046.86, Hectare: 10000 },
  },
  Speed: {
    name: 'Speed',
    units: { Meter_per_Second: 1, Kilometer_per_Hour: 0.277778, Mile_per_Hour: 0.44704, Knot: 0.514444 },
  },
  Time: {
    name: 'Time',
    units: { Second: 1, Minute: 60, Hour: 3600, Day: 86400, Week: 604800, Month: 2.628e6, Year: 3.154e7 },
  },
  Data: {
    name: 'Data',
    units: { Byte: 1, Kilobyte: 1024, Megabyte: 1.049e6, Gigabyte: 1.074e9, Terabyte: 1.1e12 },
  },
  Pressure: {
    name: 'Pressure',
    units: { Pascal: 1, Bar: 1e5, PSI: 6894.76, Atmosphere: 101325, Torr: 133.322 },
  },
  Energy: {
    name: 'Energy',
    units: { Joule: 1, Kilojoule: 1000, Calorie: 4.184, Kilocalorie: 4184, Watt_Hour: 3600, Kilowatt_Hour: 3.6e6 },
  },
};

function convert(value: number, from: string, to: string, category: string): number {
  if (category === 'Temperature') {
    let celsius = value;
    if (from === 'Fahrenheit') celsius = (value - 32) * 5 / 9;
    if (from === 'Kelvin') celsius = value - 273.15;
    if (to === 'Celsius') return celsius;
    if (to === 'Fahrenheit') return celsius * 9 / 5 + 32;
    if (to === 'Kelvin') return celsius + 273.15;
    return celsius;
  }
  const units = CATEGORIES[category].units;
  return value * (units[from] / units[to]);
}

export default function UnitConverter({ windowId: _windowId }: { windowId: string }) {
  const [category, setCategory] = useState('Length');
  const [from, setFrom] = useState('Meter');
  const [to, setTo] = useState('Kilometer');
  const [value, setValue] = useState(1);

  const units = useMemo(() => Object.keys(CATEGORIES[category].units), [category]);

  const result = useMemo(() => {
    const r = convert(value, from, to, category);
    if (Math.abs(r) < 0.000001 || Math.abs(r) > 1e12) return r.toExponential(6);
    return parseFloat(r.toPrecision(10)).toString();
  }, [value, from, to, category]);

  const swap = () => { setFrom(to); setTo(from); };

  return (
    <div className="w-full h-full flex flex-col p-4 select-none overflow-auto" style={{ background: 'var(--bg-workspace)' }}>
      <div className="flex items-center gap-2 mb-4">
        <ArrowLeftRight size={18} style={{ color: 'var(--accent-silver)' }} />
        <span className="text-sm font-medium" style={{ color: 'var(--text-primary)' }}>Unit Converter</span>
      </div>

      {/* Category */}
      <div className="grid grid-cols-5 gap-1 mb-4">
        {Object.keys(CATEGORIES).map(cat => (
          <button key={cat} onClick={() => { setCategory(cat); const u = Object.keys(CATEGORIES[cat].units); setFrom(u[0]); setTo(u[1] || u[0]); }}
            className="px-1 py-1.5 rounded-lg text-[10px] font-medium capitalize transition-all"
            style={{ background: category === cat ? 'var(--accent-dark-gray)' : 'var(--bg-input)', color: 'var(--text-primary)' }}>{cat.replace('_', ' ')}</button>
        ))}
      </div>

      {/* Conversion area */}
      <div className="flex-1 flex flex-col items-center justify-center gap-4">
        {/* From */}
        <div className="w-full max-w-xs p-4 rounded-xl" style={{ background: 'var(--bg-window)' }}>
          <div className="flex items-center justify-between mb-2">
            <select value={from} onChange={e => setFrom(e.target.value)} className="text-xs rounded px-2 py-1 outline-none bg-transparent" style={{ color: 'var(--accent-silver)' }}>
              {units.map(u => <option key={u} value={u}>{u.replace('_', ' ')}</option>)}
            </select>
          </div>
          <input type="number" value={value} onChange={e => setValue(parseFloat(e.target.value) || 0)}
            className="w-full text-2xl font-mono font-bold bg-transparent outline-none"
            style={{ color: 'var(--text-primary)' }} />
        </div>

        {/* Swap */}
        <button onClick={swap} className="p-2 rounded-full hover:bg-[var(--bg-hover)] transition-all" style={{ color: 'var(--accent-silver)' }}>
          <ArrowUpDown size={18} />
        </button>

        {/* To */}
        <div className="w-full max-w-xs p-4 rounded-xl" style={{ background: 'var(--bg-window)' }}>
          <div className="flex items-center justify-between mb-2">
            <select value={to} onChange={e => setTo(e.target.value)} className="text-xs rounded px-2 py-1 outline-none bg-transparent" style={{ color: 'var(--accent-silver)' }}>
              {units.map(u => <option key={u} value={u}>{u.replace('_', ' ')}</option>)}
            </select>
          </div>
          <div className="w-full text-2xl font-mono font-bold truncate" style={{ color: 'var(--accent-silver)' }}>{result}</div>
        </div>
      </div>
    </div>
  );
}
