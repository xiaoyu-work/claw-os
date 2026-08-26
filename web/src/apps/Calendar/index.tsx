import { useState } from 'react';
import { ChevronLeft, ChevronRight, CalendarDays } from 'lucide-react';

interface CalendarProps {
  windowId: string;
}

interface CalendarEvent {
  id: string;
  title: string;
  date: string; // YYYY-MM-DD
  time?: string;
  description?: string;
}

const initialEvents: CalendarEvent[] = [
  { id: '1', title: 'Team Standup', date: '2025-01-15', time: '09:00', description: 'Daily standup meeting' },
  { id: '2', title: 'Project Review', date: '2025-01-15', time: '14:00', description: 'Review Q1 progress' },
  { id: '3', title: 'Lunch with Sarah', date: '2025-01-17', time: '12:30' },
  { id: '4', title: 'WebOS Launch', date: '2025-01-20', time: '10:00', description: 'Release day!' },
  { id: '5', title: 'Code Review', date: '2025-01-22', time: '15:00' },
  { id: '6', title: 'Gym', date: '2025-01-24', time: '18:00' },
  { id: '7', title: 'Design Workshop', date: '2025-01-27', time: '11:00' },
];

export default function Calendar({ windowId }: CalendarProps) {
  const today = new Date();
  const [currentYear, setCurrentYear] = useState(today.getFullYear());
  const [currentMonth, setCurrentMonth] = useState(today.getMonth());
  const [selectedDay, setSelectedDay] = useState<number | null>(today.getDate());
  const [events, setEvents] = useState<CalendarEvent[]>(initialEvents);
  const [showAdd, setShowAdd] = useState(false);
  const [newTitle, setNewTitle] = useState('');
  const [newTime, setNewTime] = useState('');

  const monthNames = ['January', 'February', 'March', 'April', 'May', 'June', 'July', 'August', 'September', 'October', 'November', 'December'];
  const dayNames = ['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat'];

  const firstDay = new Date(currentYear, currentMonth, 1).getDay();
  const daysInMonth = new Date(currentYear, currentMonth + 1, 0).getDate();
  const prevMonthDays = new Date(currentYear, currentMonth, 0).getDate();

  const isToday = (day: number) => day === today.getDate() && currentMonth === today.getMonth() && currentYear === today.getFullYear();
  const dateStr = (day: number) => `${currentYear}-${String(currentMonth + 1).padStart(2, '0')}-${String(day).padStart(2, '0')}`;

  const dayEvents = selectedDay ? events.filter((e) => e.date === dateStr(selectedDay)) : [];
  const upcomingEvents = events
    .filter((e) => new Date(e.date) >= new Date(today.getFullYear(), today.getMonth(), today.getDate()))
    .sort((a, b) => new Date(a.date).getTime() - new Date(b.date).getTime())
    .slice(0, 5);

  const prevMonth = () => {
    if (currentMonth === 0) { setCurrentMonth(11); setCurrentYear(currentYear - 1); }
    else { setCurrentMonth(currentMonth - 1); }
    setSelectedDay(null);
  };
  const nextMonth = () => {
    if (currentMonth === 11) { setCurrentMonth(0); setCurrentYear(currentYear + 1); }
    else { setCurrentMonth(currentMonth + 1); }
    setSelectedDay(null);
  };

  const handleAddEvent = () => {
    if (!selectedDay || !newTitle.trim()) return;
    setEvents([...events, { id: `ev-${Date.now()}`, title: newTitle, date: dateStr(selectedDay), time: newTime }]);
    setNewTitle('');
    setNewTime('');
    setShowAdd(false);
  };

  return (
    <div className="w-full h-full flex text-sm" style={{ background: 'var(--bg-workspace)' }}>
      {/* Calendar */}
      <div className="flex-1 p-4">
        <div className="flex items-center justify-between mb-4">
          <div className="flex items-center gap-2">
            <button onClick={prevMonth} className="p-1 rounded hover:bg-[var(--bg-hover)]"><ChevronLeft size={20} /></button>
            <span className="text-base font-semibold w-40 text-center">{monthNames[currentMonth]} {currentYear}</span>
            <button onClick={nextMonth} className="p-1 rounded hover:bg-[var(--bg-hover)]"><ChevronRight size={20} /></button>
          </div>
          <button onClick={() => { setCurrentMonth(today.getMonth()); setCurrentYear(today.getFullYear()); setSelectedDay(today.getDate()); }}
            className="px-3 py-1 rounded text-xs hover:bg-[var(--bg-hover)]" style={{ background: 'var(--bg-input)' }}>
            Today
          </button>
        </div>

        <div className="grid grid-cols-7 gap-1">
          {dayNames.map((d) => (
            <div key={d} className="text-center text-xs font-medium text-[var(--text-muted)] py-1">{d}</div>
          ))}
          {Array.from({ length: firstDay }, (_, i) => (
            <div key={`prev-${i}`} className="text-center py-2 text-sm text-[var(--text-muted)] opacity-40">{prevMonthDays - firstDay + 1 + i}</div>
          ))}
          {Array.from({ length: daysInMonth }, (_, i) => {
            const day = i + 1;
            const hasEvent = events.some((e) => e.date === dateStr(day));
            return (
              <button
                key={day}
                onClick={() => { setSelectedDay(day); setShowAdd(false); }}
                className={`relative text-center py-2 text-sm rounded-lg transition-colors ${
                  isToday(day) ? 'font-bold' : ''
                } ${selectedDay === day ? 'text-white' : 'text-[var(--text-primary)] hover:bg-[var(--bg-hover)]'}`}
                style={selectedDay === day ? { background: 'var(--accent-dark-gray)' } : isToday(day) ? { background: 'rgba(125,139,150,0.2)' } : {}}
              >
                {day}
                {hasEvent && <span className="absolute bottom-1 left-1/2 -translate-x-1/2 w-1 h-1 rounded-full bg-[var(--accent-silver)]" />}
              </button>
            );
          })}
        </div>
      </div>

      {/* Sidebar */}
      <div className="w-56 p-4 border-l overflow-y-auto" style={{ borderColor: 'rgba(0,0,0,0.06)', background: 'var(--bg-window)' }}>
        {selectedDay && (
          <div className="mb-4">
            <h3 className="text-sm font-semibold mb-2">
              {monthNames[currentMonth]} {selectedDay}
            </h3>
            {dayEvents.length === 0 ? (
              <p className="text-xs text-[var(--text-muted)]">No events</p>
            ) : (
              dayEvents.map((ev) => (
                <div key={ev.id} className="p-2 rounded mb-1" style={{ background: 'var(--bg-input)' }}>
                  <div className="text-xs font-medium">{ev.title}</div>
                  {ev.time && <div className="text-[10px] text-[var(--text-muted)]">{ev.time}</div>}
                  {ev.description && <div className="text-[10px] text-[var(--text-muted)]">{ev.description}</div>}
                </div>
              ))
            )}
            {!showAdd ? (
              <button onClick={() => setShowAdd(true)} className="text-xs text-[var(--accent-silver)] hover:underline mt-1">+ Add event</button>
            ) : (
              <div className="mt-2 space-y-1">
                <input value={newTitle} onChange={(e) => setNewTitle(e.target.value)} placeholder="Event title" className="w-full h-7 px-2 rounded text-xs outline-none" style={{ background: 'var(--bg-input)', color: 'var(--text-primary)', border: '1px solid rgba(0,0,0,0.06)' }} />
                <input value={newTime} onChange={(e) => setNewTime(e.target.value)} placeholder="Time" className="w-full h-7 px-2 rounded text-xs outline-none" style={{ background: 'var(--bg-input)', color: 'var(--text-primary)', border: '1px solid rgba(0,0,0,0.06)' }} />
                <button onClick={handleAddEvent} className="w-full py-1 rounded text-xs text-white" style={{ background: 'var(--accent-dark-gray)' }}>Add</button>
              </div>
            )}
          </div>
        )}

        <div>
          <h3 className="text-sm font-semibold mb-2">Upcoming</h3>
          {upcomingEvents.map((ev) => (
            <div key={ev.id} className="flex items-start gap-2 py-1.5 border-b" style={{ borderColor: 'rgba(0,0,0,0.04)' }}>
              <CalendarDays size={14} className="text-[var(--accent-silver)] mt-0.5 shrink-0" />
              <div>
                <div className="text-xs font-medium">{ev.title}</div>
                <div className="text-[10px] text-[var(--text-muted)]">{ev.date}{ev.time ? ` at ${ev.time}` : ''}</div>
              </div>
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}
