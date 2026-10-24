import React from 'react';
import ReactDOM from 'react-dom/client';
import { QueryCache, QueryClient, QueryClientProvider } from '@tanstack/react-query';
import App from './App';
import { setToken } from './api';
import './styles.css';

const queryClient = new QueryClient({
  queryCache: new QueryCache({
    onError: (err) => {
      const e = err as Error & { status?: number };
      if (e.status === 401) {
        setToken(null);
        location.reload();
      }
    },
  }),
  defaultOptions: {
    queries: { retry: 1, staleTime: 5000, refetchOnWindowFocus: false },
  },
});

const theme = localStorage.getItem('tj-theme') || 'auto';
document.documentElement.dataset.theme = theme;

ReactDOM.createRoot(document.getElementById('root')!).render(
  <React.StrictMode>
    <QueryClientProvider client={queryClient}>
      <App />
    </QueryClientProvider>
  </React.StrictMode>,
);
