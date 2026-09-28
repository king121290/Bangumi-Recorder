"use client";

import { useEffect, useState } from "react";
import { ThemeProvider } from "next-themes";
import { WifiOff } from "lucide-react";
import { Toaster } from "sonner";
import { TooltipProvider } from "@/components/ui/tooltip";
import { AuthProvider } from "@/lib/auth-context";

export function Providers({ children }: { children: React.ReactNode }) {
  return (
    <ThemeProvider
      attribute="class"
      defaultTheme="system"
      enableSystem
      disableTransitionOnChange
    >
      <TooltipProvider delayDuration={280}>
        <AuthProvider>
          <OfflineBanner />
          {children}
          <Toaster richColors position="top-center" closeButton />
        </AuthProvider>
      </TooltipProvider>
    </ThemeProvider>
  );
}

function OfflineBanner() {
  const [offline, setOffline] = useState(false);
  useEffect(() => {
    const update = () => setOffline(!navigator.onLine);
    update();
    window.addEventListener("online", update);
    window.addEventListener("offline", update);
    return () => {
      window.removeEventListener("online", update);
      window.removeEventListener("offline", update);
    };
  }, []);
  if (!offline) return null;
  return (
    <div
      role="status"
      className="fixed inset-x-0 top-0 z-[100] flex min-h-10 items-center justify-center gap-2 bg-warning px-4 text-sm font-semibold text-warning-foreground"
    >
      <WifiOff className="size-4" />
      当前处于离线状态；已加载内容仍可查看，修改会在网络恢复后才能提交。
    </div>
  );
}
