// SPDX-License-Identifier: MIT
import * as Dialog from "@radix-ui/react-dialog";
import * as DropdownMenu from "@radix-ui/react-dropdown-menu";
import * as Popover from "@radix-ui/react-popover";
import * as Tabs from "@radix-ui/react-tabs";
import { useState } from "react";
import { useForm } from "react-hook-form";

interface ProfileFormValues {
  name: string;
  email: string;
  role: "engineer" | "designer" | "lead";
}

export function App() {
  const [activeTab, setActiveTab] = useState("account");
  const [dialogOpen, setDialogOpen] = useState(false);
  const [dialogStatus, setDialogStatus] = useState("idle");
  const [density, setDensity] = useState<"comfortable" | "compact">("comfortable");
  const [menuAction, setMenuAction] = useState("none");
  const [submitted, setSubmitted] = useState<string | null>(null);

  const {
    register,
    handleSubmit,
    formState: { errors },
  } = useForm<ProfileFormValues>({
    defaultValues: { name: "", email: "", role: "engineer" },
  });

  const onSubmit = (data: ProfileFormValues) => {
    setSubmitted(`${data.name.trim()} (${data.email.trim()}) - ${data.role}`);
  };

  return (
    <main className="mx-auto max-w-2xl p-6 space-y-5">
      <header className="flex items-center justify-between border-b border-slate-200 pb-4">
        <div>
          <h1 className="text-xl font-bold tracking-tight text-slate-900">
            Workspace Settings
          </h1>
          <p className="text-xs text-slate-500">
            Tailwind v4 + Radix UI + React Hook Form
          </p>
        </div>

        <div className="flex items-center gap-2">
          <Popover.Root>
            <Popover.Trigger asChild>
              <button
                type="button"
                data-testid="open-popover"
                className="rounded-md border border-slate-300 bg-white px-3 py-1.5 text-sm font-medium text-slate-700 hover:bg-slate-100"
              >
                Density
              </button>
            </Popover.Trigger>
            <Popover.Portal>
              <Popover.Content
                data-testid="popover-content"
                sideOffset={6}
                className="z-40 w-56 rounded-lg border border-slate-200 bg-white p-3 shadow-lg"
              >
                <div className="text-xs font-semibold text-slate-700 mb-2">
                  Layout Density
                </div>
                <button
                  type="button"
                  data-testid="popover-toggle"
                  onClick={() =>
                    setDensity((d) => (d === "comfortable" ? "compact" : "comfortable"))
                  }
                  className="w-full rounded-md bg-slate-900 px-3 py-1.5 text-xs font-medium text-white"
                >
                  Switch to {density === "comfortable" ? "compact" : "comfortable"}
                </button>
              </Popover.Content>
            </Popover.Portal>
          </Popover.Root>

          <DropdownMenu.Root>
            <DropdownMenu.Trigger asChild>
              <button
                type="button"
                data-testid="open-menu"
                className="rounded-md border border-slate-300 bg-white px-3 py-1.5 text-sm font-medium text-slate-700 hover:bg-slate-100"
              >
                Actions
              </button>
            </DropdownMenu.Trigger>
            <DropdownMenu.Portal>
              <DropdownMenu.Content
                data-testid="menu-content"
                sideOffset={6}
                className="z-40 min-w-40 rounded-lg border border-slate-200 bg-white p-1 shadow-lg"
              >
                <DropdownMenu.Item
                  data-testid="menu-item-duplicate"
                  onSelect={() => setMenuAction("duplicated")}
                  className="cursor-pointer rounded px-2.5 py-1.5 text-sm text-slate-700 outline-none focus:bg-slate-100"
                >
                  Duplicate workspace
                </DropdownMenu.Item>
                <DropdownMenu.Item
                  data-testid="menu-item-archive"
                  onSelect={() => setMenuAction("archived")}
                  className="cursor-pointer rounded px-2.5 py-1.5 text-sm text-rose-600 outline-none focus:bg-rose-50"
                >
                  Archive workspace
                </DropdownMenu.Item>
              </DropdownMenu.Content>
            </DropdownMenu.Portal>
          </DropdownMenu.Root>

          <Dialog.Root open={dialogOpen} onOpenChange={setDialogOpen}>
            <Dialog.Trigger asChild>
              <button
                type="button"
                data-testid="open-dialog"
                className="rounded-md bg-blue-600 px-3 py-1.5 text-sm font-medium text-white hover:bg-blue-700"
              >
                Deploy
              </button>
            </Dialog.Trigger>
            <Dialog.Portal>
              <Dialog.Overlay className="fixed inset-0 z-40 bg-slate-950/40" />
              <Dialog.Content
                data-testid="dialog-content"
                className="fixed left-1/2 top-1/2 z-50 w-96 -translate-x-1/2 -translate-y-1/2 rounded-xl border border-slate-200 bg-white p-5 shadow-xl"
              >
                <Dialog.Title className="text-base font-semibold text-slate-900">
                  Confirm deployment
                </Dialog.Title>
                <Dialog.Description className="mt-1 text-sm text-slate-600">
                  Publish the current workspace configuration to production.
                </Dialog.Description>
                <div className="mt-4 flex justify-end gap-2">
                  <Dialog.Close asChild>
                    <button
                      type="button"
                      data-testid="dialog-cancel"
                      className="rounded-md border border-slate-300 px-3 py-1.5 text-sm text-slate-700"
                    >
                      Cancel
                    </button>
                  </Dialog.Close>
                  <button
                    type="button"
                    data-testid="dialog-confirm"
                    onClick={() => {
                      setDialogStatus("deployed");
                      setDialogOpen(false);
                    }}
                    className="rounded-md bg-blue-600 px-3 py-1.5 text-sm font-medium text-white"
                  >
                    Confirm
                  </button>
                </div>
              </Dialog.Content>
            </Dialog.Portal>
          </Dialog.Root>
        </div>
      </header>

      <section className="grid grid-cols-3 gap-3 text-xs">
        <div className="rounded-lg border border-slate-200 bg-white p-3">
          <div className="text-slate-500">Deploy status</div>
          <div data-testid="dialog-status" className="mt-1 font-semibold text-slate-900">
            {dialogStatus}
          </div>
        </div>
        <div className="rounded-lg border border-slate-200 bg-white p-3">
          <div className="text-slate-500">Density</div>
          <div data-testid="density-value" className="mt-1 font-semibold text-slate-900">
            {density}
          </div>
        </div>
        <div className="rounded-lg border border-slate-200 bg-white p-3">
          <div className="text-slate-500">Last menu action</div>
          <div data-testid="menu-action" className="mt-1 font-semibold text-slate-900">
            {menuAction}
          </div>
        </div>
      </section>

      <Tabs.Root value={activeTab} onValueChange={setActiveTab} className="space-y-3">
        <Tabs.List className="inline-flex rounded-lg bg-slate-200/70 p-1">
          <Tabs.Trigger
            value="account"
            data-testid="tab-account"
            className="rounded-md px-3 py-1 text-sm font-medium text-slate-700 data-[state=active]:bg-white data-[state=active]:text-slate-900 data-[state=active]:shadow-xs"
          >
            Account
          </Tabs.Trigger>
          <Tabs.Trigger
            value="security"
            data-testid="tab-security"
            className="rounded-md px-3 py-1 text-sm font-medium text-slate-700 data-[state=active]:bg-white data-[state=active]:text-slate-900 data-[state=active]:shadow-xs"
          >
            Security
          </Tabs.Trigger>
          <Tabs.Trigger
            value="prefs"
            data-testid="tab-prefs"
            className="rounded-md px-3 py-1 text-sm font-medium text-slate-700 data-[state=active]:bg-white data-[state=active]:text-slate-900 data-[state=active]:shadow-xs"
          >
            Preferences
          </Tabs.Trigger>
        </Tabs.List>

        <Tabs.Content
          value="account"
          data-testid="panel-account"
          className="rounded-xl border border-slate-200 bg-white p-4"
        >
          <form
            data-testid="profile-form"
            onSubmit={handleSubmit(onSubmit)}
            noValidate
            className="space-y-3"
          >
            <div>
              <label htmlFor="name" className="block text-xs font-medium text-slate-700">
                Full name
              </label>
              <input
                id="name"
                data-testid="input-name"
                {...register("name", {
                  required: "Name is required",
                  minLength: { value: 3, message: "Name must be at least 3 characters" },
                })}
                className="mt-1 w-full rounded-md border border-slate-300 px-3 py-1.5 text-sm"
                placeholder="Ada Lovelace"
              />
              {errors.name && (
                <p data-testid="error-name" className="mt-1 text-xs text-rose-600">
                  {errors.name.message}
                </p>
              )}
            </div>

            <div>
              <label htmlFor="email" className="block text-xs font-medium text-slate-700">
                Email
              </label>
              <input
                id="email"
                type="email"
                data-testid="input-email"
                {...register("email", {
                  required: "Email is required",
                  pattern: {
                    value: /^[^\s@]+@[^\s@]+\.[^\s@]+$/,
                    message: "Enter a valid email",
                  },
                })}
                className="mt-1 w-full rounded-md border border-slate-300 px-3 py-1.5 text-sm"
                placeholder="ada@squark.dev"
              />
              {errors.email && (
                <p data-testid="error-email" className="mt-1 text-xs text-rose-600">
                  {errors.email.message}
                </p>
              )}
            </div>

            <div className="flex items-end gap-3">
              <div className="flex-1">
                <label htmlFor="role" className="block text-xs font-medium text-slate-700">
                  Role
                </label>
                <select
                  id="role"
                  data-testid="select-role"
                  {...register("role")}
                  className="mt-1 w-full rounded-md border border-slate-300 bg-white px-3 py-1.5 text-sm"
                >
                  <option value="engineer">Engineer</option>
                  <option value="designer">Designer</option>
                  <option value="lead">Lead</option>
                </select>
              </div>
              <button
                type="submit"
                data-testid="submit-form"
                className="rounded-md bg-slate-900 px-4 py-1.5 text-sm font-medium text-white hover:bg-slate-800"
              >
                Save profile
              </button>
            </div>

            {submitted && (
              <div
                data-testid="form-result"
                className="rounded-md border border-emerald-200 bg-emerald-50 px-3 py-2 text-xs font-medium text-emerald-800"
              >
                Saved: {submitted}
              </div>
            )}
          </form>
        </Tabs.Content>

        <Tabs.Content
          value="security"
          data-testid="panel-security"
          className="rounded-xl border border-slate-200 bg-white p-4 text-sm text-slate-700"
        >
          Two-factor authentication is enabled for this workspace.
        </Tabs.Content>

        <Tabs.Content
          value="prefs"
          data-testid="panel-prefs"
          className="rounded-xl border border-slate-200 bg-white p-4 text-sm text-slate-700"
        >
          Current density preference: <strong data-testid="prefs-density">{density}</strong>.
        </Tabs.Content>
      </Tabs.Root>
    </main>
  );
}
