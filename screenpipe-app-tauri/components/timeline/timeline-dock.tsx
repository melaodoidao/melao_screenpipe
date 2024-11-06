import React, { useEffect, useState, useCallback, useMemo } from "react";
import { motion, AnimatePresence } from "framer-motion";
import { cn } from "@/lib/utils";
import { platform } from "@tauri-apps/plugin-os";
import { invoke } from "@tauri-apps/api/core";
import { TimeBlock, TimelineBlock } from "@/app/timeline/page";
import { debounce } from "lodash";

interface TimelineDockProps {
  children: React.ReactNode;
  magnification?: number;
  distance?: number;
  className?: string;
}

interface TimelineDockIconProps {
  children: React.ReactNode;
  className?: string;
  timestamp?: Date;
  appName?: string;
  mouseX?: number | null;
  index?: number;
  magnification?: number;
  distance?: number;
  style?: React.CSSProperties;
}

export function TimelineDock({
  children,
  magnification = 2,
  distance = 100,
  className,
}: TimelineDockProps) {
  const [mouseX, setMouseX] = React.useState<null | number>(null);

  // Debounce mouse move handler
  const handleMouseMove = useMemo(
    () =>
      debounce((e: React.MouseEvent) => {
        const bounds = e.currentTarget.getBoundingClientRect();
        setMouseX(e.clientX - bounds.left);
      }, 16),  // ~60fps
    []
  );

  return (
    <motion.div
      onMouseMove={handleMouseMove}
      onMouseLeave={() => setMouseX(null)}
      className={cn(
        "flex items-center justify-start h-8 relative px-2 w-full",
        className
      )}
    >
      {useMemo(() => 
        React.Children.map(children, (child, index) => {
          if (!React.isValidElement(child)) return null;
          return React.cloneElement(
            child as React.ReactElement<TimelineDockIconProps>,
            {
              mouseX,
              index,
              magnification,
              distance,
            }
          );
        }),
        [children, mouseX, magnification, distance]
      )}
    </motion.div>
  );
}

// Memoize the TimelineDockIcon component
export const TimelineDockIcon = React.memo(function TimelineDockIcon({
  children,
  className,
  timestamp,
  appName,
  mouseX,
  index,
  magnification = 2,
  distance = 50,
  style,
}: TimelineDockIconProps) {
  const ref = React.useRef<HTMLDivElement>(null);

  // Calculate scale based on percentage position rather than pixel position
  let scale = 1;
  let isClosest = false;
  let spacing = 0;

  if (mouseX != null && style?.left) {
    // Convert left percentage to actual position
    const iconPosition = parseFloat(style.left as string);
    const mousePosition =
      (mouseX / (ref.current?.parentElement?.offsetWidth || 1)) * 100;

    // Calculate distance as percentage difference
    const distanceFromMouse = Math.abs(mousePosition - iconPosition);

    if (distanceFromMouse < 5) {
      // Reduced distance threshold to 5%
      scale = Math.max(
        1,
        magnification - (distanceFromMouse / 5) * magnification
      );

      spacing = scale > 1 ? 2 * (1 - distanceFromMouse / 5) : 0;
      isClosest = distanceFromMouse < 2; // Show tooltip when within 2%
    }
  }

  const yOffset = scale > 1 ? (scale - 1) * 16 : 0;
  const xOffset = mouseX != null ? spacing : 0;

  return (
    <motion.div
      ref={ref}
      style={{
        scale,
        y: -yOffset,
        x: xOffset,
        zIndex: scale > 1 ? 1 : 0,
        position: "absolute",
        transform: `translate(-50%, -50%)`,
        ...style,
      }}
      transition={{
        type: "spring",
        stiffness: 400,
        damping: 25,
      }}
      className={cn(
        "flex items-center justify-center w-4 h-4 origin-center",
        className
      )}
    >
      <div className="relative flex items-center justify-center">
        {children}
        <AnimatePresence>
          {isClosest && (
            <motion.div
              initial={{ opacity: 0, y: -10 }}
              animate={{ opacity: 1, y: 0 }}
              exit={{ opacity: 0, y: -10 }}
              className="absolute top-full mt-2 left-1/2 -translate-x-1/2 flex flex-col items-center bg-background/80 backdrop-blur-sm px-2 py-1 rounded-md border shadow-sm text-xs whitespace-nowrap"
            >
              <span>{appName}</span>
              <span>{timestamp?.toLocaleTimeString()}</span>
            </motion.div>
          )}
        </AnimatePresence>
      </div>
    </motion.div>
  );
});

// Optimize TimelineIconsSection
export function TimelineIconsSection({ blocks }: { blocks: TimelineBlock[] }) {
  const [iconCache, setIconCache] = useState<{ [key: string]: string }>({});

  // Memoize significant blocks calculation
  const significantBlocks = useMemo(() => 
    blocks.filter((block) => {
      const minutesInDay = 24 * 60;
      const blockMinutes = (block.width / 100) * minutesInDay;
      return blockMinutes > 10;
    }),
    [blocks]
  );

  // Memoize unique apps calculation
  const uniqueApps = useMemo(() => 
    [...new Set(significantBlocks.map((block) => block.appName))],
    [significantBlocks]
  );

  const loadAppIcon = useCallback(async (appName: string, appPath?: string) => {
    if (iconCache[appName]) return;
    
    const icon = await invoke<{ base64: string; path: string } | null>(
      "get_app_icon",
      { appName, appPath }
    );

    if (icon) {
      setIconCache(prev => ({
        ...prev,
        [appName]: icon.base64,
      }));
    }
  }, [iconCache]);

  useEffect(() => {
    const loadIcons = async () => {
      const p = platform();
      if (p !== "macos") return;

      // Load icons for unique app names only
      uniqueApps.forEach((appName) => {
        loadAppIcon(appName);
      });
    };

    loadIcons();
  }, [uniqueApps, loadAppIcon]);

  return (
    <div className="absolute -bottom-20 inset-x-0 pointer-events-none">
      {significantBlocks.map((block, i) => (
        <div
          key={`${block.appName}-${i}`}
          className="absolute top-0 h-full"
          style={{
            left: `${block.left}%`,
            width: `${block.width}%`,
          }}
        >
          {iconCache[block.appName] && (
            <img
              src={`data:image/png;base64,${iconCache[block.appName]}`}
              className="w-6 h-6 absolute top-1/2 left-1/2 -translate-x-1/2 -translate-y-1/2 opacity-50"
              alt={block.appName}
              loading="lazy"
              decoding="async"
            />
          )}
        </div>
      ))}
    </div>
  );
}