// SPDX-License-Identifier: MIT
import { useEffect, useState } from "react";

type Filter = "all" | "active" | "completed";

interface Todo {
  id: number;
  title: string;
  completed: boolean;
}

const STORAGE_KEY = "squark-todomvc-v1";

function filterFromHash(): Filter {
  const hash = window.location.hash;
  if (hash === "#/active") return "active";
  if (hash === "#/completed") return "completed";
  return "all";
}

function loadInitialTodos(): Todo[] {
  try {
    const raw = window.localStorage.getItem(STORAGE_KEY);
    if (!raw) return [];
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    return parsed.filter(
      (item): item is Todo =>
        typeof item === "object" &&
        item !== null &&
        typeof (item as Todo).id === "number" &&
        typeof (item as Todo).title === "string" &&
        typeof (item as Todo).completed === "boolean",
    );
  } catch {
    return [];
  }
}

export function App() {
  const [todos, setTodos] = useState<Todo[]>(loadInitialTodos);
  const [draft, setDraft] = useState("");
  const [filter, setFilter] = useState<Filter>(filterFromHash);
  const [editingId, setEditingId] = useState<number | null>(null);
  const [editText, setEditText] = useState("");

  useEffect(() => {
    try {
      window.localStorage.setItem(STORAGE_KEY, JSON.stringify(todos));
    } catch {
      // Ignore storage errors when disabled.
    }
  }, [todos]);

  useEffect(() => {
    const onHash = () => setFilter(filterFromHash());
    window.addEventListener("hashchange", onHash);
    return () => window.removeEventListener("hashchange", onHash);
  }, []);

  const activeCount = todos.filter((t) => !t.completed).length;
  const completedCount = todos.length - activeCount;
  const visibleTodos = todos.filter((t) => {
    if (filter === "active") return !t.completed;
    if (filter === "completed") return t.completed;
    return true;
  });

  function addTodo() {
    const title = draft.trim();
    if (!title) return;
    setTodos((prev) => {
      const nextId = prev.reduce((max, item) => Math.max(max, item.id), 0) + 1;
      return [...prev, { id: nextId, title, completed: false }];
    });
    setDraft("");
  }

  function toggleTodo(id: number) {
    setTodos((prev) =>
      prev.map((t) => (t.id === id ? { ...t, completed: !t.completed } : t)),
    );
  }

  function toggleAll(checked: boolean) {
    setTodos((prev) => prev.map((t) => ({ ...t, completed: checked })));
  }

  function removeTodo(id: number) {
    setTodos((prev) => prev.filter((t) => t.id !== id));
  }

  function clearCompleted() {
    setTodos((prev) => prev.filter((t) => !t.completed));
  }

  function startEdit(todo: Todo) {
    setEditingId(todo.id);
    setEditText(todo.title);
  }

  function commitEdit(id: number) {
    const trimmed = editText.trim();
    if (!trimmed) {
      removeTodo(id);
    } else {
      setTodos((prev) =>
        prev.map((t) => (t.id === id ? { ...t, title: trimmed } : t)),
      );
    }
    setEditingId(null);
  }

  function selectFilter(next: Filter) {
    setFilter(next);
    window.location.hash = next === "all" ? "#/" : `#/${next}`;
  }

  return (
    <section className="todoapp">
      <header className="header">
        <h1>todos</h1>
        <input
          className="new-todo"
          data-testid="new-todo"
          placeholder="What needs to be done?"
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              addTodo();
            }
          }}
        />
      </header>

      {todos.length > 0 && (
        <section className="main">
          <label className="toggle-all-label">
            <input
              type="checkbox"
              className="toggle-all"
              data-testid="toggle-all"
              checked={activeCount === 0}
              onChange={(e) => toggleAll(e.target.checked)}
            />
            <span>Mark all as complete</span>
          </label>

          <ul className="todo-list" data-testid="todo-list">
            {visibleTodos.map((todo) => {
              const isEditing = editingId === todo.id;
              return (
                <li
                  key={todo.id}
                  className={[
                    "todo-item",
                    todo.completed ? "completed" : "",
                    isEditing ? "editing" : "",
                  ]
                    .filter(Boolean)
                    .join(" ")}
                  data-testid="todo-item"
                  data-id={todo.id}
                >
                  {isEditing ? (
                    <input
                      className="edit"
                      data-testid="todo-edit"
                      autoFocus
                      value={editText}
                      onChange={(e) => setEditText(e.target.value)}
                      onBlur={() => commitEdit(todo.id)}
                      onKeyDown={(e) => {
                        if (e.key === "Enter") {
                          commitEdit(todo.id);
                        } else if (e.key === "Escape") {
                          setEditingId(null);
                        }
                      }}
                    />
                  ) : (
                    <div className="view">
                      <input
                        type="checkbox"
                        className="toggle"
                        data-testid="todo-toggle"
                        checked={todo.completed}
                        onChange={() => toggleTodo(todo.id)}
                      />
                      <label
                        className="todo-label"
                        data-testid="todo-label"
                        onDoubleClick={() => startEdit(todo)}
                      >
                        {todo.title}
                      </label>
                      <button
                        type="button"
                        className="destroy"
                        data-testid="todo-destroy"
                        aria-label={`Delete ${todo.title}`}
                        onClick={() => removeTodo(todo.id)}
                      >
                        ×
                      </button>
                    </div>
                  )}
                </li>
              );
            })}
          </ul>
        </section>
      )}

      {todos.length > 0 && (
        <footer className="footer" data-testid="todo-footer">
          <span className="todo-count" data-testid="todo-count">
            <strong>{activeCount}</strong> {activeCount === 1 ? "item" : "items"} left
          </span>
          <ul className="filters">
            <li>
              <a
                href="#/"
                className={filter === "all" ? "selected" : ""}
                data-testid="filter-all"
                onClick={(e) => {
                  e.preventDefault();
                  selectFilter("all");
                }}
              >
                All
              </a>
            </li>
            <li>
              <a
                href="#/active"
                className={filter === "active" ? "selected" : ""}
                data-testid="filter-active"
                onClick={(e) => {
                  e.preventDefault();
                  selectFilter("active");
                }}
              >
                Active
              </a>
            </li>
            <li>
              <a
                href="#/completed"
                className={filter === "completed" ? "selected" : ""}
                data-testid="filter-completed"
                onClick={(e) => {
                  e.preventDefault();
                  selectFilter("completed");
                }}
              >
                Completed
              </a>
            </li>
          </ul>
          {completedCount > 0 && (
            <button
              type="button"
              className="clear-completed"
              data-testid="clear-completed"
              onClick={clearCompleted}
            >
              Clear completed
            </button>
          )}
        </footer>
      )}
    </section>
  );
}
